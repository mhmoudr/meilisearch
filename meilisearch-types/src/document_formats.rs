use std::fmt::{self, Debug, Display};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Write};
use std::marker::PhantomData;

use milli::documents::Error;
use milli::Object;
use serde::de::{SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::error::Category;

use crate::error::{Code, ErrorCode};

type Result<T> = std::result::Result<T, DocumentFormatError>;

#[derive(Debug)]
pub enum PayloadType {
    Ndjson,
    Json,
    Csv { delimiter: u8 },
}

impl fmt::Display for PayloadType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PayloadType::Ndjson => f.write_str("ndjson"),
            PayloadType::Json => f.write_str("json"),
            PayloadType::Csv { .. } => f.write_str("csv"),
        }
    }
}

#[derive(Debug)]
pub enum DocumentFormatError {
    Io(io::Error),
    MalformedPayload(Error, PayloadType),
}

impl Display for DocumentFormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::MalformedPayload(me, b) => match me {
                Error::Json(se) => {
                    let mut message = match se.classify() {
                        Category::Data => {
                            "data are neither an object nor a list of objects".to_string()
                        }
                        _ => se.to_string(),
                    };

                    // https://github.com/meilisearch/meilisearch/issues/2107
                    // The user input maybe insanely long. We need to truncate it.
                    let ellipsis = "...";
                    let trim_input_prefix_len = 50;
                    let trim_input_suffix_len = 85;

                    if message.len()
                        > trim_input_prefix_len + trim_input_suffix_len + ellipsis.len()
                    {
                        message.replace_range(
                            trim_input_prefix_len..message.len() - trim_input_suffix_len,
                            ellipsis,
                        );
                    }

                    write!(
                        f,
                        "The `{}` payload provided is malformed. `Couldn't serialize document value: {}`.",
                        b, message
                    )
                }
                _ => write!(f, "The `{}` payload provided is malformed: `{}`.", b, me),
            },
        }
    }
}

impl std::error::Error for DocumentFormatError {}

impl From<(PayloadType, Error)> for DocumentFormatError {
    fn from((ty, error): (PayloadType, Error)) -> Self {
        match error {
            Error::Io(e) => Self::Io(e),
            e => Self::MalformedPayload(e, ty),
        }
    }
}

impl From<io::Error> for DocumentFormatError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl ErrorCode for DocumentFormatError {
    fn error_code(&self) -> Code {
        match self {
            DocumentFormatError::Io(e) => e.error_code(),
            DocumentFormatError::MalformedPayload(_, _) => Code::MalformedPayload,
        }
    }
}

/// Reads CSV from input and write an obkv batch to writer.
pub fn read_csv<F: Write>(
    _input: BufReader<File>,
    _output: &mut BufWriter<F>,
    _delimiter: u8,
) -> Result<u64> {
    todo!()
    // let mut builder = DocumentsBatchBuilder::new(BufWriter::new(output));
    // let mmap = unsafe { MmapOptions::new().map(input)? };
    // let csv = csv::ReaderBuilder::new().delimiter(delimiter).from_reader(mmap.as_ref());
    // builder.append_csv(csv).map_err(|e| (PayloadType::Csv { delimiter }, e))?;

    // let count = builder.documents_count();
    // let _ = builder.into_inner().map_err(DocumentFormatError::Io)?;

    // Ok(count as u64)
}

/// Reads JSON from temporary file and write an obkv batch to writer.
pub fn read_json<F: Write>(input: BufReader<File>, mut output: &mut BufWriter<F>) -> Result<u64> {
    let mut count = 0;
    let mut deserializer = serde_json::Deserializer::from_reader(input);
    match array_each(&mut deserializer, |obj: Object| {
        count += 1;
        serde_json::to_writer(&mut output, &obj)
    }) {
        // The json data has been deserialized and does not need to be processed again.
        // The data has been transferred to the writer during the deserialization process.
        Ok(Ok(_)) => (),
        Ok(Err(e)) => return Err(DocumentFormatError::Io(e.into())),
        Err(e) => {
            // Attempt to deserialize a single json string when the cause of the exception is not Category.data
            // Other types of deserialisation exceptions are returned directly to the front-end
            if e.classify() != serde_json::error::Category::Data {
                return Err(DocumentFormatError::MalformedPayload(
                    Error::Json(e),
                    PayloadType::Json,
                ));
            }

            todo!("single document/object update")

            // let content: Object = serde_json::from_slice(&mmap)
            //     .map_err(Error::Json)
            //     .map_err(|e| (PayloadType::Json, e))?;
            // serde_json::to_writer(&mut output, &content).unwrap()
        }
    }

    Ok(count)
}

/// Reads JSON from temporary file and write it into the writer.
pub fn read_ndjson<F: Write>(input: BufReader<File>, mut output: &mut BufWriter<F>) -> Result<u64> {
    let mut count = 0;
    for result in serde_json::Deserializer::from_reader(input).into_iter() {
        count += 1;
        // TODO Correctly manage the errors
        //      Avoid copying the content: use CowStr from milli (move it elsewhere)
        let map: Object = result.unwrap();
        serde_json::to_writer(&mut output, &map).unwrap();
    }

    Ok(count)
}

/// The actual handling of the deserialization process in serde
/// avoids storing the deserialized object in memory.
///
/// ## References
/// <https://serde.rs/stream-array.html>
/// <https://github.com/serde-rs/json/issues/160>
fn array_each<'de, D, T, F>(
    deserializer: D,
    f: F,
) -> std::result::Result<serde_json::Result<u64>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
    F: FnMut(T) -> serde_json::Result<()>,
{
    struct SeqVisitor<T, F>(F, PhantomData<T>);

    impl<'de, T, F> Visitor<'de> for SeqVisitor<T, F>
    where
        T: Deserialize<'de>,
        F: FnMut(T) -> serde_json::Result<()>,
    {
        type Value = serde_json::Result<u64>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a nonempty sequence")
        }

        fn visit_seq<A>(
            mut self,
            mut seq: A,
        ) -> std::result::Result<serde_json::Result<u64>, <A as SeqAccess<'de>>::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut max: u64 = 0;
            while let Some(value) = seq.next_element::<T>()? {
                match self.0(value) {
                    Ok(()) => max += 1,
                    Err(e) => return Ok(Err(e)),
                };
            }
            Ok(Ok(max))
        }
    }
    let visitor = SeqVisitor(f, PhantomData);
    deserializer.deserialize_seq(visitor)
}
