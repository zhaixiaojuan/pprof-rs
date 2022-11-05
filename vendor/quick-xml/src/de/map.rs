//! Serde `Deserializer` module

use crate::{
    de::escape::EscapedDeserializer,
    de::{DeEvent, Deserializer, XmlRead, INNER_VALUE, UNFLATTEN_PREFIX},
    errors::serialize::DeError,
    events::attributes::IterState,
    events::BytesStart,
};
use serde::de::{self, DeserializeSeed, IntoDeserializer};
use std::borrow::Cow;
use std::ops::Range;

/// Representing state of the `MapAccess` accessor.
enum State {
    /// `next_key_seed` not yet called. This is initial state and state after deserializing
    /// value (calling `next_value_seed`).
    Empty,
    /// `next_key_seed` checked the attributes list and find it is not exhausted yet.
    /// Next call to the `next_value_seed` will deserialize type from the attribute value
    Attribute(Range<usize>),
    /// The same as `InnerValue`
    Nested,
    /// Value should be deserialized from the text content of the XML node:
    ///
    /// ```xml
    /// <...>text content for field value<...>
    /// ```
    InnerValue,
}

/// A deserializer for `Attributes`
pub(crate) struct MapAccess<'de, 'a, R>
where
    R: XmlRead<'de>,
{
    /// Tag -- owner of attributes
    start: BytesStart<'de>,
    de: &'a mut Deserializer<'de, R>,
    /// State of the iterator over attributes. Contains the next position in the
    /// inner `start` slice, from which next attribute should be parsed.
    iter: IterState,
    /// Current state of the accessor that determines what next call to API
    /// methods should return.
    state: State,
    /// list of fields yet to unflatten (defined as starting with $unflatten=)
    unflatten_fields: Vec<&'static [u8]>,
}

impl<'de, 'a, R> MapAccess<'de, 'a, R>
where
    R: XmlRead<'de>,
{
    /// Create a new MapAccess
    pub fn new(
        de: &'a mut Deserializer<'de, R>,
        start: BytesStart<'de>,
        fields: &[&'static str],
    ) -> Result<Self, DeError> {
        Ok(MapAccess {
            de,
            start,
            iter: IterState::new(0, false),
            state: State::Empty,
            unflatten_fields: fields
                .iter()
                .filter(|f| f.starts_with(UNFLATTEN_PREFIX))
                .map(|f| f.as_bytes())
                .collect(),
        })
    }
}

impl<'de, 'a, R> de::MapAccess<'de> for MapAccess<'de, 'a, R>
where
    R: XmlRead<'de>,
{
    type Error = DeError;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Self::Error> {
        // FIXME: There error positions counted from end of tag name - need global position
        let slice = self.start.attributes_raw();
        let decoder = self.de.reader.decoder();
        let has_value_field = self.de.has_value_field;

        if let Some(a) = self.iter.next(slice).transpose()? {
            // try getting map from attributes (key= "value")
            let (key, value) = a.into();
            self.state = State::Attribute(value.unwrap_or_default());
            seed.deserialize(EscapedDeserializer::new(
                Cow::Borrowed(&slice[key]),
                decoder,
                false,
            ))
            .map(Some)
        } else {
            // try getting from events (<key>value</key>)
            match self.de.peek()? {
                DeEvent::Text(_) | DeEvent::CData(_) => {
                    self.state = State::InnerValue;
                    // Deserialize `key` from special attribute name which means
                    // that value should be taken from the text content of the
                    // XML node
                    seed.deserialize(INNER_VALUE.into_deserializer()).map(Some)
                }
                // Used to deserialize collections of enums, like:
                // <root>
                //   <A/>
                //   <B/>
                //   <C/>
                // </root>
                //
                // into
                //
                // enum Enum { A, B, С }
                // struct Root {
                //     #[serde(rename = "$value")]
                //     items: Vec<Enum>,
                // }
                // TODO: This should be handled by #[serde(flatten)]
                // See https://github.com/serde-rs/serde/issues/1905
                DeEvent::Start(_) if has_value_field => {
                    self.state = State::InnerValue;
                    seed.deserialize(INNER_VALUE.into_deserializer()).map(Some)
                }
                DeEvent::Start(e) => {
                    let key = if let Some(p) = self
                        .unflatten_fields
                        .iter()
                        .position(|f| e.name() == &f[UNFLATTEN_PREFIX.len()..])
                    {
                        // Used to deserialize elements, like:
                        // <root>
                        //   <xxx>test</xxx>
                        // </root>
                        //
                        // into
                        //
                        // struct Root {
                        //     #[serde(rename = "$unflatten=xxx")]
                        //     xxx: String,
                        // }
                        self.state = State::InnerValue;
                        seed.deserialize(self.unflatten_fields.remove(p).into_deserializer())
                    } else {
                        let name = Cow::Borrowed(e.local_name());
                        self.state = State::Nested;
                        seed.deserialize(EscapedDeserializer::new(name, decoder, false))
                    };
                    key.map(Some)
                }
                _ => Ok(None),
            }
        }
    }

    fn next_value_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<K::Value, Self::Error> {
        match std::mem::replace(&mut self.state, State::Empty) {
            State::Attribute(value) => {
                let slice = self.start.attributes_raw();
                let decoder = self.de.reader.decoder();

                seed.deserialize(EscapedDeserializer::new(
                    Cow::Borrowed(&slice[value]),
                    decoder,
                    true,
                ))
            }
            State::Nested | State::InnerValue => seed.deserialize(&mut *self.de),
            State::Empty => Err(DeError::EndOfAttributes),
        }
    }
}
