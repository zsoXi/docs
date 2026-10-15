//! PP-OCRv5 Latin adapter, transferred from the offline reference implementation.
//! Sessions belong to an isolated worker. No downloads or engine selection.
use image::{GrayImage, Luma, Rgb, RgbImage, imageops};
use imageproc::{
    contours::{BorderType, find_contours},
    geometric_transformations::{Border, Interpolation, Projection, warp_into},
    geometry::min_area_rect,
};
use ndarray::Array4;
use ort::{session::Session, value::Tensor};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{}", crate::messages::OCR_RUNTIME_FAILURE)]
    Runtime(#[from] ort::LoadDynamicError),
    #[error("{}", crate::messages::OCR_INFERENCE_FAILURE)]
    Ort(#[from] ort::Error),
    #[error("{}", crate::messages::OCR_DATA_FAILURE)]
    Invalid,
}

/// Coordinates in original-image pixels: top-left, top-right, bottom-right, bottom-left.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub text: String,
    pub bbox: [[f32; 2]; 4],
    /// Mean probability of emitted (non-blank, non-repeated) CTC symbols.
    pub confidence: f32,
}

/// Owns two mutable ORT sessions. Use one adapter per worker, not a stale shared copy.
pub struct PaddleOcr {
    det: Session,
    rec: Session,
    dictionary: Vec<String>,
}

/// Explicit initialization avoids ort's implicit panic on a missing dynamic library.
/// The library is executable code: this path must be trusted by the application.
pub fn load_runtime(path: &Path) -> Result<(), Error> {
    ort::init_from(path)?.with_name("docs-ocr").commit();
    Ok(())
}

impl PaddleOcr {
    pub fn load(
        runtime: &Path,
        detector: &[u8],
        recognizer: &[u8],
        dictionary: &str,
    ) -> Result<Self, Error> {
        load_runtime(runtime)?;
        let dictionary = parse_dictionary(dictionary)?;
        let session = |bytes: &[u8]| -> Result<Session, Error> {
            Ok(Session::builder()?
                .with_intra_threads(1)
                .map_err(ort::Error::from)?
                .with_inter_threads(1)
                .map_err(ort::Error::from)?
                .commit_from_memory(bytes)?)
        };
        Ok(Self {
            det: session(detector)?,
            rec: session(recognizer)?,
            dictionary,
        })
    }

    /// Recognize upright text, including small rotations. Not a document-layout engine.
    /// Bounds cap preprocessing allocations. A Result is essential for external input.
    pub fn recognize(&mut self, image: &RgbImage) -> Result<Vec<Line>, Error> {
        validate_image(image)?;
        let (width, height) = detection_size(image.width(), image.height());
        let resized = imageops::resize(image, width, height, imageops::FilterType::Triangle);
        let input =
            Array4::from_shape_fn((1, 3, height as usize, width as usize), |(_, c, y, x)| {
                // Official Paddle DecodeImage uses BGR, then ImageNet normalization.
                let value = resized.get_pixel(x as u32, y as u32)[2 - c] as f32 / 255.;
                (value - [0.485, 0.456, 0.406][c]) / [0.229, 0.224, 0.225][c]
            });
        let outputs = self.det.run(ort::inputs![Tensor::from_array(input)?])?;
        let (shape, data) = outputs[0].try_extract_tensor::<f32>()?;
        if shape.len() != 4 || shape[0] != 1 || shape[1] != 1 || shape[2] <= 0 || shape[3] <= 0 {
            return Err(Error::Invalid);
        }
        let mh = u32::try_from(shape[2]).map_err(|_| Error::Invalid)?;
        let mw = u32::try_from(shape[3]).map_err(|_| Error::Invalid)?;
        if mw > 2048 || mh > 2048 || u64::from(mw) * u64::from(mh) != data.len() as u64 {
            return Err(Error::Invalid);
        }
        validate_probabilities(data)?;
        let (threshold, box_threshold, unclip) = (0.3, 0.6, 1.5);
        let bitmap = GrayImage::from_fn(mw, mh, |x, y| {
            Luma([if data[(y * mw + x) as usize] > threshold {
                255
            } else {
                0
            }])
        });
        let mut boxes = Vec::new();
        for contour in find_contours::<i32>(&bitmap)
            .into_iter()
            .filter(|c| c.border_type == BorderType::Outer)
            .take(3000)
        {
            if contour.points.len() < 4 {
                continue;
            }
            let rect = min_area_rect(&contour.points);
            let mut quad = rect.map(|p| [p.x as f32, p.y as f32]);
            order_quad(&mut quad);
            let w = distance(quad[0], quad[1]);
            let h = distance(quad[0], quad[3]);
            if w < 3. || h < 3. {
                continue;
            }
            // Score pixels inside the rotated rectangle, not its enclosing AABB.
            let minx = quad
                .iter()
                .map(|p| p[0])
                .fold(f32::INFINITY, f32::min)
                .max(0.) as u32;
            let maxx = quad
                .iter()
                .map(|p| p[0])
                .fold(0., f32::max)
                .min((mw - 1) as f32) as u32;
            let miny = quad
                .iter()
                .map(|p| p[1])
                .fold(f32::INFINITY, f32::min)
                .max(0.) as u32;
            let maxy = quad
                .iter()
                .map(|p| p[1])
                .fold(0., f32::max)
                .min((mh - 1) as f32) as u32;
            let mut score = 0.;
            let mut count = 0;
            for y in miny..=maxy {
                for x in minx..=maxx {
                    if inside_quad([x as f32, y as f32], &quad) {
                        score += data[(y * mw + x) as usize];
                        count += 1;
                    }
                }
            }
            if count == 0 || score / (count as f32) < box_threshold {
                continue;
            }
            // DB distance = area * ratio / perimeter; rectangular offset approximation.
            let offset = w * h * unclip / (2. * (w + h));
            let ux = [(quad[1][0] - quad[0][0]) / w, (quad[1][1] - quad[0][1]) / w];
            let uy = [(quad[3][0] - quad[0][0]) / h, (quad[3][1] - quad[0][1]) / h];
            for (p, (sx, sy)) in quad
                .iter_mut()
                .zip([(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)])
            {
                p[0] = ((p[0] + offset * (sx * ux[0] + sy * uy[0])) * image.width() as f32
                    / mw as f32)
                    .clamp(0., (image.width() - 1) as f32);
                p[1] = ((p[1] + offset * (sx * ux[1] + sy * uy[1])) * image.height() as f32
                    / mh as f32)
                    .clamp(0., (image.height() - 1) as f32);
            }
            boxes.push(quad);
        }
        drop(outputs);
        let mut lines = Vec::new();
        // Batched recognition in width-sorted groups reduces zero-padding.
        let mut crops = boxes
            .into_iter()
            .map(|b| rectify(image, b).map(|im| (b, im)))
            .collect::<Result<Vec<_>, _>>()?;
        crops.sort_by_key(|(_, im)| im.width());
        for batch in crops.chunks(4) {
            let bw = batch
                .iter()
                .map(|(_, im)| {
                    ((im.width() as f32 * 48. / im.height() as f32).ceil() as u32).clamp(1, 3200)
                })
                .max()
                .unwrap_or(320)
                .max(320);
            let mut tensor = Array4::<f32>::zeros((batch.len(), 3, 48, bw as usize));
            for (n, (_, im)) in batch.iter().enumerate() {
                let rw =
                    ((im.width() as f32 * 48. / im.height() as f32).ceil() as u32).clamp(1, bw);
                let resized = imageops::resize(im, rw, 48, imageops::FilterType::Triangle);
                for y in 0..48 {
                    for x in 0..rw {
                        for c in 0..3 {
                            tensor[[n, c, y as usize, x as usize]] =
                                resized.get_pixel(x, y)[2 - c] as f32 / 127.5 - 1.;
                        }
                    }
                }
            }
            let output = self.rec.run(ort::inputs![Tensor::from_array(tensor)?])?;
            let (shape, probs) = output[0].try_extract_tensor::<f32>()?;
            if shape.len() != 3
                || shape[0] != batch.len() as i64
                || shape[1] <= 0
                || shape[2] != self.dictionary.len() as i64
            {
                return Err(Error::Invalid);
            }
            let steps = shape[1] as usize;
            let classes = self.dictionary.len();
            let stride = steps.checked_mul(classes).ok_or(Error::Invalid)?;
            if stride.checked_mul(batch.len()) != Some(probs.len()) {
                return Err(Error::Invalid);
            }
            for (n, (bbox, _)) in batch.iter().enumerate() {
                let (text, confidence) = decode_ctc(
                    &probs[n * stride..(n + 1) * stride],
                    classes,
                    &self.dictionary,
                )?;
                if !text.is_empty() {
                    lines.push(Line {
                        text,
                        bbox: *bbox,
                        confidence,
                    });
                }
            }
        }
        // Group aligned cells left-to-right; column segmentation remains a layout task.
        sort_reading_order(&mut lines);
        Ok(lines)
    }
}

fn sort_reading_order(lines: &mut [Line]) {
    let center_y = |line: &Line| line.bbox.iter().map(|p| p[1]).sum::<f32>() / 4.;
    let height = |line: &Line| distance(line.bbox[0], line.bbox[3]);
    lines.sort_by(|a, b| center_y(a).total_cmp(&center_y(b)));
    let mut start = 0;
    while start < lines.len() {
        let anchor = center_y(&lines[start]);
        let anchor_height = height(&lines[start]);
        let mut end = start + 1;
        while end < lines.len()
            && (center_y(&lines[end]) - anchor).abs()
                <= 0.5 * anchor_height.min(height(&lines[end]))
        {
            end += 1;
        }
        lines[start..end].sort_by(|a, b| a.bbox[0][0].total_cmp(&b.bbox[0][0]));
        start = end;
    }
}

fn validate_image(image: &RgbImage) -> Result<(), Error> {
    let (w, h) = image.dimensions();
    if w == 0 || h == 0 || w > 10000 || h > 10000 || u64::from(w) * u64::from(h) > 40_000_000 {
        return Err(Error::Invalid);
    }
    Ok(())
}

fn detection_size(w: u32, h: u32) -> (u32, u32) {
    let scale = 960. / w.max(h) as f32;
    let round = |v: u32| ((v as f32 * scale / 32.).round().max(1.) as u32) * 32;
    (round(w), round(h))
}

fn parse_dictionary(text: &str) -> Result<Vec<String>, Error> {
    let mut dict = vec![String::new()]; // CTC blank at zero.
    for line in text.lines() {
        if line.is_empty() {
            return Err(Error::Invalid);
        }
        dict.push(line.to_owned());
    }
    if dict.len() < 2 {
        return Err(Error::Invalid);
    }
    // Paddle adds a space after the supplied character_dict.
    dict.push(" ".into());
    Ok(dict)
}

fn validate_probabilities(probs: &[f32]) -> Result<(), Error> {
    if probs
        .iter()
        .any(|p| !p.is_finite() || !(0.0..=1.00001).contains(p))
    {
        return Err(Error::Invalid);
    }
    Ok(())
}

fn decode_ctc(probs: &[f32], classes: usize, dict: &[String]) -> Result<(String, f32), Error> {
    if classes < 2
        || classes != dict.len()
        || probs.is_empty()
        || !probs.len().is_multiple_of(classes)
    {
        return Err(Error::Invalid);
    }
    validate_probabilities(probs)?;
    let mut text = String::new();
    let mut previous = 0;
    let mut sum = 0.;
    let mut count = 0;
    for row in probs.chunks_exact(classes) {
        let (index, &prob) = row
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .ok_or(Error::Invalid)?;
        if index != 0 && index != previous {
            text.push_str(&dict[index]);
            sum += prob;
            count += 1;
        }
        previous = index;
    }
    Ok((text, if count > 0 { sum / count as f32 } else { 0. }))
}

fn distance(a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}
fn order_quad(q: &mut [[f32; 2]; 4]) {
    let cx = q.iter().map(|p| p[0]).sum::<f32>() / 4.;
    let cy = q.iter().map(|p| p[1]).sum::<f32>() / 4.;
    q.sort_by(|a, b| {
        (a[1] - cy)
            .atan2(a[0] - cx)
            .total_cmp(&(b[1] - cy).atan2(b[0] - cx))
    });
    let start = q
        .iter()
        .enumerate()
        .min_by(|a, b| (a.1[0] + a.1[1]).total_cmp(&(b.1[0] + b.1[1])))
        .map_or(0, |v| v.0);
    q.rotate_left(start);
}
fn inside_quad(p: [f32; 2], q: &[[f32; 2]; 4]) -> bool {
    (0..4).all(|i| {
        let a = q[i];
        let b = q[(i + 1) % 4];
        (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]) >= 0.
    })
}
fn rectify(image: &RgbImage, q: [[f32; 2]; 4]) -> Result<RgbImage, Error> {
    let w = distance(q[0], q[1])
        .max(distance(q[3], q[2]))
        .round()
        .clamp(1., 10000.) as u32;
    let h = distance(q[0], q[3])
        .max(distance(q[1], q[2]))
        .round()
        .clamp(1., 10000.) as u32;
    if u64::from(w) * u64::from(h) > 40_000_000 {
        return Err(Error::Invalid);
    }
    let projection = Projection::from_control_points(
        q.map(|p| (p[0], p[1])),
        [
            (0., 0.),
            (w as f32 - 1., 0.),
            (w as f32 - 1., h as f32 - 1.),
            (0., h as f32 - 1.),
        ],
    )
    .ok_or(Error::Invalid)?;
    let mut crop = RgbImage::new(w, h);
    warp_into(
        image,
        projection,
        Interpolation::Bilinear,
        Border::Constant(Rgb([255; 3])),
        &mut crop,
    );
    Ok(crop)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ctc_polish_repeats_and_blank() -> Result<(), Error> {
        let dict = parse_dictionary("Ą\nć\nł\n")?;
        let ids = [1, 1, 0, 1, 2, 2, 3, 4];
        let mut p = vec![0.; ids.len() * dict.len()];
        for (t, id) in ids.into_iter().enumerate() {
            p[t * dict.len() + id] = 1.;
        }
        assert_eq!(decode_ctc(&p, dict.len(), &dict)?, ("ĄĄćł ".into(), 1.));
        Ok(())
    }
    #[test]
    fn rejects_nonfinite_probability() {
        assert!(validate_probabilities(&[f32::NAN]).is_err());
        assert!(validate_probabilities(&[f32::INFINITY]).is_err());
        assert!(validate_probabilities(&[-0.1]).is_err());
        assert!(validate_probabilities(&[1.1]).is_err());
    }
    #[test]
    fn rejects_mismatched_dictionary() -> Result<(), Error> {
        let dict = parse_dictionary("ą\n")?;
        assert!(decode_ctc(&[0.1, 0.9], 2, &dict).is_err());
        assert!(decode_ctc(&[0.1], 3, &dict).is_err());
        assert!(parse_dictionary("").is_err());
        assert!(parse_dictionary("a\n\nb\n").is_err());
        Ok(())
    }
    #[test]
    fn rejects_empty_image() {
        assert!(validate_image(&RgbImage::new(0, 1)).is_err());
    }
    #[test]
    fn aligned_cells_read_left_to_right_despite_height_jitter() {
        let line = |text: &str, x, y| Line {
            text: text.into(),
            bbox: [[x, y], [x + 20., y], [x + 20., y + 20.], [x, y + 20.]],
            confidence: 1.,
        };
        let mut lines = vec![
            line("right", 100., 10.),
            line("next", 10., 80.),
            line("left", 10., 13.),
        ];
        sort_reading_order(&mut lines);
        assert_eq!(
            lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(),
            ["left", "right", "next"]
        );
    }
    #[test]
    fn size_is_multiple_of_32() {
        assert_eq!(detection_size(2000, 3000), (640, 960));
        assert_eq!(detection_size(1, 10000), (32, 960));
    }
    #[test]
    fn rectification_preserves_pixels() -> Result<(), Error> {
        let image = RgbImage::from_pixel(40, 20, Rgb([10, 20, 30]));
        let crop = rectify(&image, [[2., 2.], [30., 2.], [30., 15.], [2., 15.]])?;
        assert_eq!(*crop.get_pixel(5, 5), Rgb([10, 20, 30]));
        assert!(rectify(&image, [[0., 0.]; 4]).is_err());
        Ok(())
    }
}
