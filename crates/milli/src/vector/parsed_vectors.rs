use std::collections::{BTreeMap, BTreeSet};

use deserr::{take_cf_content, DeserializeError, Deserr, Sequence};
use obkv::KvReader;
use serde_json::value::RawValue;
use serde_json::{from_slice, Value};

use super::Embedding;
use crate::index::IndexEmbeddingConfig;
use crate::update::del_add::{DelAdd, KvReaderDelAdd};
use crate::{DocumentId, FieldId, InternalError, UserError};

pub const RESERVED_VECTORS_FIELD_NAME: &str = "_vectors";

#[derive(serde::Serialize, Debug)]
#[serde(untagged)]
pub enum RawVectors<'doc> {
    Explicit(#[serde(borrow)] RawExplicitVectors<'doc>),
    ImplicitlyUserProvided(#[serde(borrow)] Option<&'doc RawValue>),
}

impl<'doc> RawVectors<'doc> {
    pub fn from_raw_value(raw: &'doc RawValue) -> Result<Self, serde_json::Error> {
        use serde::de::Deserializer as _;
        Ok(match raw.deserialize_any(RawVectorsVisitor)? {
            RawVectorsVisitorValue::ImplicitNone => RawVectors::ImplicitlyUserProvided(None),
            RawVectorsVisitorValue::Implicit => RawVectors::ImplicitlyUserProvided(Some(raw)),
            RawVectorsVisitorValue::Explicit { regenerate, embeddings } => {
                RawVectors::Explicit(RawExplicitVectors { embeddings, regenerate })
            }
        })
    }
}

struct RawVectorsVisitor;

enum RawVectorsVisitorValue<'doc> {
    ImplicitNone,
    Implicit,
    Explicit { regenerate: bool, embeddings: Option<&'doc RawValue> },
}

impl<'doc> serde::de::Visitor<'doc> for RawVectorsVisitor {
    type Value = RawVectorsVisitorValue<'doc>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(formatter, "a map containing at least `regenerate`, or an array of floats`")
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(RawVectorsVisitorValue::ImplicitNone)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'doc>,
    {
        deserializer.deserialize_any(self)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(RawVectorsVisitorValue::ImplicitNone)
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::SeqAccess<'doc>,
    {
        // must consume all elements or parsing fails
        while let Some(_) = seq.next_element::<&RawValue>()? {}
        Ok(RawVectorsVisitorValue::Implicit)
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::MapAccess<'doc>,
    {
        use serde::de::Error as _;
        let mut regenerate = None;
        let mut embeddings = None;
        while let Some(s) = map.next_key()? {
            match s {
                "regenerate" => {
                    let value: bool = map.next_value()?;
                    regenerate = Some(value);
                }
                "embeddings" => {
                    let value: &RawValue = map.next_value()?;
                    embeddings = Some(value);
                }
                other => return Err(A::Error::unknown_field(other, &["regenerate", "embeddings"])),
            }
        }
        let Some(regenerate) = regenerate else {
            return Err(A::Error::missing_field("regenerate"));
        };
        Ok(RawVectorsVisitorValue::Explicit { regenerate, embeddings })
    }
}

#[derive(serde::Serialize, Debug)]
#[serde(untagged)]
pub enum Vectors {
    ImplicitlyUserProvided(VectorOrArrayOfVectors),
    Explicit(ExplicitVectors),
}

impl<E: DeserializeError> Deserr<E> for Vectors {
    fn deserialize_from_value<V: deserr::IntoValue>(
        value: deserr::Value<V>,
        location: deserr::ValuePointerRef<'_>,
    ) -> Result<Self, E> {
        match value {
            deserr::Value::Sequence(_) | deserr::Value::Null => {
                Ok(Vectors::ImplicitlyUserProvided(VectorOrArrayOfVectors::deserialize_from_value(
                    value, location,
                )?))
            }
            deserr::Value::Map(_) => {
                Ok(Vectors::Explicit(ExplicitVectors::deserialize_from_value(value, location)?))
            }

            value => Err(take_cf_content(E::error(
                None,
                deserr::ErrorKind::IncorrectValueKind {
                    actual: value,
                    accepted: &[
                        deserr::ValueKind::Sequence,
                        deserr::ValueKind::Map,
                        deserr::ValueKind::Null,
                    ],
                },
                location,
            ))),
        }
    }
}

impl Vectors {
    pub fn must_regenerate(&self) -> bool {
        match self {
            Vectors::ImplicitlyUserProvided(_) => false,
            Vectors::Explicit(ExplicitVectors { regenerate, .. }) => *regenerate,
        }
    }

    pub fn into_array_of_vectors(self) -> Option<Vec<Embedding>> {
        match self {
            Vectors::ImplicitlyUserProvided(embeddings) => {
                Some(embeddings.into_array_of_vectors().unwrap_or_default())
            }
            Vectors::Explicit(ExplicitVectors { embeddings, regenerate: _ }) => {
                embeddings.map(|embeddings| embeddings.into_array_of_vectors().unwrap_or_default())
            }
        }
    }
}

impl<'doc> RawVectors<'doc> {
    pub fn must_regenerate(&self) -> bool {
        match self {
            RawVectors::ImplicitlyUserProvided(_) => false,
            RawVectors::Explicit(RawExplicitVectors { regenerate, .. }) => *regenerate,
        }
    }
    pub fn embeddings(&self) -> Option<&'doc RawValue> {
        match self {
            RawVectors::ImplicitlyUserProvided(embeddings) => *embeddings,
            RawVectors::Explicit(RawExplicitVectors { embeddings, regenerate: _ }) => *embeddings,
        }
    }
}

#[derive(serde::Serialize, Deserr, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ExplicitVectors {
    #[serde(default)]
    #[deserr(default)]
    pub embeddings: Option<VectorOrArrayOfVectors>,
    pub regenerate: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RawExplicitVectors<'doc> {
    #[serde(borrow)]
    #[serde(default)]
    pub embeddings: Option<&'doc RawValue>,
    pub regenerate: bool,
}

pub enum VectorState {
    Inline(Vectors),
    Manual,
    Generated,
}

impl VectorState {
    pub fn must_regenerate(&self) -> bool {
        match self {
            VectorState::Inline(vectors) => vectors.must_regenerate(),
            VectorState::Manual => false,
            VectorState::Generated => true,
        }
    }
}

pub enum VectorsState {
    NoVectorsFid,
    NoVectorsFieldInDocument,
    Vectors(BTreeMap<String, Vectors>),
}

pub struct ParsedVectorsDiff {
    old: BTreeMap<String, VectorState>,
    new: VectorsState,
}

impl ParsedVectorsDiff {
    pub fn new(
        docid: DocumentId,
        embedders_configs: &[IndexEmbeddingConfig],
        documents_diff: &KvReader<FieldId>,
        old_vectors_fid: Option<FieldId>,
        new_vectors_fid: Option<FieldId>,
    ) -> Result<Self, Error> {
        let mut old = match old_vectors_fid
            .and_then(|vectors_fid| documents_diff.get(vectors_fid))
            .map(|bytes| to_vector_map(bytes.into(), DelAdd::Deletion))
            .transpose()
        {
            Ok(del) => del,
            // ignore wrong shape for old version of documents, use an empty map in this case
            Err(Error::InvalidMap(value)) => {
                tracing::warn!(%value, "Previous version of the `_vectors` field had a wrong shape");
                Default::default()
            }
            Err(error) => {
                return Err(error);
            }
        }
        .flatten().map_or(BTreeMap::default(), |del| del.into_iter().map(|(name, vec)| (name, VectorState::Inline(vec))).collect());
        for embedding_config in embedders_configs {
            if embedding_config.user_provided.contains(docid) {
                old.entry(embedding_config.name.to_string()).or_insert(VectorState::Manual);
            }
        }

        let new = 'new: {
            let Some(new_vectors_fid) = new_vectors_fid else {
                break 'new VectorsState::NoVectorsFid;
            };
            let Some(bytes) = documents_diff.get(new_vectors_fid) else {
                break 'new VectorsState::NoVectorsFieldInDocument;
            };
            match to_vector_map(bytes.into(), DelAdd::Addition)? {
                Some(new) => VectorsState::Vectors(new),
                None => VectorsState::NoVectorsFieldInDocument,
            }
        };

        Ok(Self { old, new })
    }

    pub fn remove(&mut self, embedder_name: &str) -> (VectorState, VectorState) {
        let old = self.old.remove(embedder_name).unwrap_or(VectorState::Generated);
        let state_from_old = match old {
            // assume a userProvided is still userProvided
            VectorState::Manual => VectorState::Manual,
            // generated is still generated
            VectorState::Generated => VectorState::Generated,
            // weird case that shouldn't happen were the previous docs version is inline,
            // but it was removed in the new version
            // Since it is not in the new version, we switch to generated
            VectorState::Inline(_) => VectorState::Generated,
        };
        let new = match &mut self.new {
            VectorsState::Vectors(new) => {
                new.remove(embedder_name).map(VectorState::Inline).unwrap_or(state_from_old)
            }
            _ =>
            // if no `_vectors` field is present in the new document,
            // the state depends on the previous version of the document
            {
                state_from_old
            }
        };

        (old, new)
    }

    pub fn into_new_vectors_keys_iter(self) -> impl Iterator<Item = String> {
        let maybe_it = match self.new {
            VectorsState::NoVectorsFid => None,
            VectorsState::NoVectorsFieldInDocument => None,
            VectorsState::Vectors(vectors) => Some(vectors.into_keys()),
        };
        maybe_it.into_iter().flatten()
    }
}

pub struct ParsedVectors(pub BTreeMap<String, Vectors>);

impl<E: DeserializeError> Deserr<E> for ParsedVectors {
    fn deserialize_from_value<V: deserr::IntoValue>(
        value: deserr::Value<V>,
        location: deserr::ValuePointerRef<'_>,
    ) -> Result<Self, E> {
        let value = <BTreeMap<String, Vectors>>::deserialize_from_value(value, location)?;
        Ok(ParsedVectors(value))
    }
}

impl ParsedVectors {
    pub fn from_bytes(value: &[u8]) -> Result<Self, Error> {
        let value: serde_json::Value = from_slice(value).map_err(Error::InternalSerdeJson)?;
        deserr::deserialize(value).map_err(|error| Error::InvalidEmbedderConf { error })
    }

    pub fn retain_not_embedded_vectors(&mut self, embedders: &BTreeSet<String>) {
        self.0.retain(|k, _v| !embedders.contains(k))
    }
}

pub enum Error {
    InvalidMap(Value),
    InvalidEmbedderConf { error: deserr::errors::JsonError },
    InternalSerdeJson(serde_json::Error),
}

impl Error {
    pub fn to_crate_error(self, document_id: String) -> crate::Error {
        match self {
            Error::InvalidMap(value) => {
                crate::Error::UserError(UserError::InvalidVectorsMapType { document_id, value })
            }
            Error::InvalidEmbedderConf { error } => {
                crate::Error::UserError(UserError::InvalidVectorsEmbedderConf {
                    document_id,
                    error: error.to_string(),
                })
            }
            Error::InternalSerdeJson(error) => {
                crate::Error::InternalError(InternalError::SerdeJson(error))
            }
        }
    }
}

fn to_vector_map(
    obkv: &KvReaderDelAdd,
    side: DelAdd,
) -> Result<Option<BTreeMap<String, Vectors>>, Error> {
    Ok(if let Some(value) = obkv.get(side) {
        let ParsedVectors(parsed_vectors) = ParsedVectors::from_bytes(value)?;
        Some(parsed_vectors)
    } else {
        None
    })
}

/// Represents either a vector or an array of multiple vectors.
#[derive(serde::Serialize, Debug)]
#[serde(transparent)]
pub struct VectorOrArrayOfVectors {
    #[serde(with = "either::serde_untagged_optional")]
    inner: Option<either::Either<Vec<Embedding>, Embedding>>,
}

impl<E: DeserializeError> Deserr<E> for VectorOrArrayOfVectors {
    fn deserialize_from_value<V: deserr::IntoValue>(
        value: deserr::Value<V>,
        location: deserr::ValuePointerRef<'_>,
    ) -> Result<Self, E> {
        match value {
            deserr::Value::Null => Ok(VectorOrArrayOfVectors { inner: None }),
            deserr::Value::Sequence(seq) => {
                let mut iter = seq.into_iter();
                match iter.next().map(|v| v.into_value()) {
                    None => {
                        // With the strange way serde serialize the `Either`, we must send the left part
                        // otherwise it'll consider we returned [[]]
                        Ok(VectorOrArrayOfVectors { inner: Some(either::Either::Left(Vec::new())) })
                    }
                    Some(val @ deserr::Value::Sequence(_)) => {
                        let first = Embedding::deserialize_from_value(val, location.push_index(0))?;
                        let mut collect = vec![first];
                        let mut tail = iter
                            .enumerate()
                            .map(|(i, v)| {
                                Embedding::deserialize_from_value(
                                    v.into_value(),
                                    location.push_index(i + 1),
                                )
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        collect.append(&mut tail);

                        Ok(VectorOrArrayOfVectors { inner: Some(either::Either::Left(collect)) })
                    }
                    Some(
                        val @ deserr::Value::Integer(_)
                        | val @ deserr::Value::NegativeInteger(_)
                        | val @ deserr::Value::Float(_),
                    ) => {
                        let first = <f32>::deserialize_from_value(val, location.push_index(0))?;
                        let mut embedding = iter
                            .enumerate()
                            .map(|(i, v)| {
                                <f32>::deserialize_from_value(
                                    v.into_value(),
                                    location.push_index(i + 1),
                                )
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        embedding.insert(0, first);
                        Ok(VectorOrArrayOfVectors { inner: Some(either::Either::Right(embedding)) })
                    }
                    Some(value) => Err(take_cf_content(E::error(
                        None,
                        deserr::ErrorKind::IncorrectValueKind {
                            actual: value,
                            accepted: &[deserr::ValueKind::Sequence, deserr::ValueKind::Float],
                        },
                        location.push_index(0),
                    ))),
                }
            }
            value => Err(take_cf_content(E::error(
                None,
                deserr::ErrorKind::IncorrectValueKind {
                    actual: value,
                    accepted: &[deserr::ValueKind::Sequence, deserr::ValueKind::Null],
                },
                location,
            ))),
        }
    }
}

impl VectorOrArrayOfVectors {
    pub fn into_array_of_vectors(self) -> Option<Vec<Embedding>> {
        match self.inner? {
            either::Either::Left(vectors) => Some(vectors),
            either::Either::Right(vector) => Some(vec![vector]),
        }
    }

    pub fn from_array_of_vectors(array_of_vec: Vec<Embedding>) -> Self {
        Self { inner: Some(either::Either::Left(array_of_vec)) }
    }

    pub fn from_vector(vec: Embedding) -> Self {
        Self { inner: Some(either::Either::Right(vec)) }
    }
}

impl From<Embedding> for VectorOrArrayOfVectors {
    fn from(vec: Embedding) -> Self {
        Self::from_vector(vec)
    }
}

impl From<Vec<Embedding>> for VectorOrArrayOfVectors {
    fn from(vec: Vec<Embedding>) -> Self {
        Self::from_array_of_vectors(vec)
    }
}

#[cfg(test)]
mod test {
    use super::VectorOrArrayOfVectors;

    fn embedding_from_str(s: &str) -> Result<VectorOrArrayOfVectors, deserr::errors::JsonError> {
        let value: serde_json::Value = serde_json::from_str(s).unwrap();
        deserr::deserialize(value)
    }

    #[test]
    fn array_of_vectors() {
        let null = embedding_from_str("null").unwrap();
        let empty = embedding_from_str("[]").unwrap();
        let one = embedding_from_str("[0.1]").unwrap();
        let two = embedding_from_str("[0.1, 0.2]").unwrap();
        let one_vec = embedding_from_str("[[0.1, 0.2]]").unwrap();
        let two_vecs = embedding_from_str("[[0.1, 0.2], [0.3, 0.4]]").unwrap();

        insta::assert_json_snapshot!(null.into_array_of_vectors(), @"null");
        insta::assert_json_snapshot!(empty.into_array_of_vectors(), @"[]");
        insta::assert_json_snapshot!(one.into_array_of_vectors(), @r###"
        [
          [
            0.1
          ]
        ]
        "###);
        insta::assert_json_snapshot!(two.into_array_of_vectors(), @r###"
        [
          [
            0.1,
            0.2
          ]
        ]
        "###);
        insta::assert_json_snapshot!(one_vec.into_array_of_vectors(), @r###"
        [
          [
            0.1,
            0.2
          ]
        ]
        "###);
        insta::assert_json_snapshot!(two_vecs.into_array_of_vectors(), @r###"
        [
          [
            0.1,
            0.2
          ],
          [
            0.3,
            0.4
          ]
        ]
        "###);
    }
}
