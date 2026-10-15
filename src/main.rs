use docs::messages::{MessageCode, params, render};
use docs::{Error, Options, run};
use std::io::{Read, Write};

fn main() {
    let result = real_main();
    let failed = result.is_err();
    let value = match result {
        Ok(value) => value,
        Err(error) => serde_json::json!(error.to_output("en")),
    };
    // A closed stdout cannot receive an error response.
    if writeln!(std::io::stdout().lock(), "{value}").is_err() || failed {
        std::process::exit(1);
    }
}
fn cli_io(source: std::io::Error) -> Error {
    Error::Reader {
        code: MessageCode::CliIo,
        source: Box::new(source),
    }
}
fn real_main() -> Result<serde_json::Value, Error> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    for (index, argument) in arguments.iter().enumerate() {
        if argument.to_str().is_none() {
            return Err(Error::invalid(
                MessageCode::CliArgumentEncoding,
                params(&[("field", format!("/arguments/{index}").into())]),
            ));
        }
    }
    if arguments.len() > 1 {
        return Err(Error::invalid(
            MessageCode::CliArguments,
            params(&[
                ("field", "/arguments".into()),
                (
                    "allowed",
                    serde_json::json!(["json", "--schema", "--help", "--messages"]),
                ),
            ]),
        ));
    }
    let first = arguments.into_iter().next();
    match first.as_ref().and_then(|s| s.to_str()) {
        Some("--schema") => return Ok(docs::schema()),
        Some("--messages") => return Ok(docs::messages::catalog()),
        Some("--help" | "-h") => {
            return Ok(
                serde_json::json!({"ok":true,"summary":render(MessageCode::CliUsage,&params(&[("name",docs::NAME.into())]),"en"),"summary_code":MessageCode::CliUsage.as_str(),"summary_params":{"name":docs::NAME},"warnings":[]}),
            );
        }
        _ => {}
    }
    let raw = match first {
        Some(json) => json.into_string().map_err(|_| {
            Error::invalid(
                MessageCode::CliArgumentEncoding,
                params(&[("field", "/arguments/0".into())]),
            )
        })?,
        None => {
            let mut buffer = String::new();
            std::io::stdin()
                .take(101 * 1024 * 1024)
                .read_to_string(&mut buffer)
                .map_err(cli_io)?;
            buffer
        }
    };
    let input = docs::parse_input(&raw)?;
    let options = Options {
        ocr_models: std::env::var_os("DOCS_OCR_MODELS").map(std::path::PathBuf::from),
        ..Options::new(std::env::current_dir().map_err(cli_io)?)
    };
    Ok(serde_json::to_value(run(&input, &options)?)?)
}
