//! Copy-on-write Form instances for editing individual Do invocations.
use crate::{
    Error,
    content::{self, Operand, Operator},
    pdfbudget,
};
use lopdf::{Dictionary, Document, ObjectId, Stream};
use std::collections::HashSet;

pub(super) fn isolate(
    doc: &mut Document,
    page: ObjectId,
    data: Vec<u8>,
    resources: Dictionary,
) -> Result<(), Error> {
    let mut instances = Instances {
        doc,
        active: HashSet::new(),
        bytes: 0,
        count: 0,
    };
    let (data, resources, changed) = instances.expand(data, resources, 0)?;
    if changed {
        let id = instances
            .doc
            .add_object(Stream::new(Dictionary::new(), data));
        let page = instances.doc.get_dictionary_mut(page)?;
        page.set("Contents", id);
        page.set("Resources", resources);
    }
    Ok(())
}

struct Instances<'a> {
    doc: &'a mut Document,
    active: HashSet<ObjectId>,
    bytes: usize,
    count: usize,
}

impl Instances<'_> {
    fn expand(
        &mut self,
        data: Vec<u8>,
        mut resources: Dictionary,
        depth: usize,
    ) -> Result<(Vec<u8>, Dictionary, bool), Error> {
        self.bytes = self.bytes.saturating_add(data.len());
        if self.bytes > pdfbudget::DOCUMENT_BYTES {
            return Err(pdfbudget::limit(
                "expanded_page_bytes",
                pdfbudget::DOCUMENT_BYTES,
            ));
        }
        let mut objects = match resources.get(b"XObject") {
            Ok(value) => self.doc.dereference(value)?.1.as_dict()?.clone(),
            Err(_) => return Ok((data, resources, false)),
        };
        let mut edits = Vec::new();
        for op in content::parse(&data)? {
            if !op.is("Do") {
                continue;
            }
            let Some(name) = op.name(0) else { continue };
            let Ok(value) = objects.get(name) else {
                continue;
            };
            let (id, object) = self.doc.dereference(value)?;
            let stream = object.as_stream()?;
            if !stream
                .dict
                .get(b"Subtype")
                .is_ok_and(|v| v.as_name().is_ok_and(|v| v == b"Form"))
            {
                continue;
            }
            self.count += 1;
            if self.count > pdfbudget::OBJECTS {
                return Err(pdfbudget::limit("form_instances", pdfbudget::OBJECTS));
            }
            if depth >= pdfbudget::DEPTH || id.is_some_and(|id| self.active.contains(&id)) {
                // Leave unvisited content untouched; the boundary reports it.
                continue;
            }
            if let Some(id) = id { self.active.insert(id); }
            let mut stream = stream.clone();
            let form_resources = match stream.dict.get(b"Resources") {
                Ok(value) => self.doc.dereference(value)?.1.as_dict()?.clone(),
                Err(_) => resources.clone(),
            };
            let (content, form_resources, _) =
                self.expand(pdfbudget::stream(&stream)?, form_resources, depth + 1)?;
            if let Some(id) = id {
                self.active.remove(&id);
            }
            stream.dict.remove(b"Filter");
            stream.dict.remove(b"DecodeParms");
            stream.dict.set("Resources", form_resources);
            stream.set_content(content);
            let id = self.doc.add_object(stream);
            let mut serial = self.count;
            let name = loop {
                let name = format!("DocsForm{serial}").into_bytes();
                if !objects.has(&name) {
                    break name;
                }
                serial += 1;
            };
            objects.set(name.clone(), id);
            let replacement = Operator {
                keyword: b"Do".to_vec(),
                operands: vec![Operand::Name(name)],
                start: 0,
                end: 0,
            };
            edits.push((op.start..op.end, super::serialize_operator(&replacement)));
        }
        let changed = !edits.is_empty();
        if changed {
            resources.set("XObject", objects);
        }
        Ok((content::splice(&data, &edits), resources, changed))
    }
}
