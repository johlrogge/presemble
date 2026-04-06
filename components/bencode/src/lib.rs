/// A bencode value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bytes(Vec<u8>),
    Integer(i64),
    List(Vec<Value>),
    Dict(Vec<(Vec<u8>, Value)>), // preserve insertion order
}

/// Error returned when decoding fails.
#[derive(Debug, PartialEq)]
pub struct DecodeError(pub String);

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "bencode decode error: {}", self.0)
    }
}

impl std::error::Error for DecodeError {}

impl Value {
    /// Return inner bytes if this is a `Bytes` variant.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        if let Value::Bytes(b) = self {
            Some(b)
        } else {
            None
        }
    }

    /// Try to interpret inner bytes as a UTF-8 string.
    pub fn as_str(&self) -> Option<&str> {
        self.as_bytes()
            .and_then(|b| std::str::from_utf8(b).ok())
    }

    /// Return inner integer if this is an `Integer` variant.
    pub fn as_integer(&self) -> Option<i64> {
        if let Value::Integer(n) = self {
            Some(*n)
        } else {
            None
        }
    }

    /// Return inner list if this is a `List` variant.
    pub fn as_list(&self) -> Option<&[Value]> {
        if let Value::List(l) = self {
            Some(l)
        } else {
            None
        }
    }

    /// Return inner dict if this is a `Dict` variant.
    pub fn as_dict(&self) -> Option<&[(Vec<u8>, Value)]> {
        if let Value::Dict(d) = self {
            Some(d)
        } else {
            None
        }
    }

    /// Look up a key in a `Dict` variant by string key.
    pub fn get(&self, key: &str) -> Option<&Value> {
        let key_bytes = key.as_bytes();
        self.as_dict()
            .and_then(|pairs| pairs.iter().find(|(k, _)| k.as_slice() == key_bytes))
            .map(|(_, v)| v)
    }

    /// Convenience constructor: build a `Dict` from string-key / value pairs.
    pub fn dict(pairs: Vec<(&str, Value)>) -> Self {
        Value::Dict(
            pairs
                .into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
        )
    }

    /// Convenience constructor: build a `Bytes` value from a string slice.
    pub fn string(s: &str) -> Self {
        Value::Bytes(s.as_bytes().to_vec())
    }
}

// ---------------------------------------------------------------------------
// Encoder
// ---------------------------------------------------------------------------

/// Encode a `Value` into its bencode byte representation.
pub fn encode(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    encode_into(value, &mut out);
    out
}

fn encode_into(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Bytes(b) => {
            out.extend_from_slice(b.len().to_string().as_bytes());
            out.push(b':');
            out.extend_from_slice(b);
        }
        Value::Integer(n) => {
            out.push(b'i');
            out.extend_from_slice(n.to_string().as_bytes());
            out.push(b'e');
        }
        Value::List(items) => {
            out.push(b'l');
            for item in items {
                encode_into(item, out);
            }
            out.push(b'e');
        }
        Value::Dict(pairs) => {
            out.push(b'd');
            for (key, val) in pairs {
                // Keys are always byte strings.
                out.extend_from_slice(key.len().to_string().as_bytes());
                out.push(b':');
                out.extend_from_slice(key);
                encode_into(val, out);
            }
            out.push(b'e');
        }
    }
}

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

/// Decode a bencode value from `input`.
///
/// Returns the decoded value and the number of bytes consumed.
pub fn decode(input: &[u8]) -> Result<(Value, usize), DecodeError> {
    if input.is_empty() {
        return Err(DecodeError("unexpected end of input".into()));
    }
    match input[0] {
        b'i' => decode_integer(input),
        b'l' => decode_list(input),
        b'd' => decode_dict(input),
        b'0'..=b'9' => decode_bytes(input),
        other => Err(DecodeError(format!(
            "unexpected byte 0x{other:02x} at start of value"
        ))),
    }
}

/// Parse `i<digits>e` starting at `input[0]`.
fn decode_integer(input: &[u8]) -> Result<(Value, usize), DecodeError> {
    // input[0] == b'i'
    let end = input[1..]
        .iter()
        .position(|&b| b == b'e')
        .ok_or_else(|| DecodeError("integer missing closing 'e'".into()))?;
    // end is relative to input[1..], so total consumed = 1 + end + 1
    let num_bytes = &input[1..1 + end];
    let num_str =
        std::str::from_utf8(num_bytes).map_err(|_| DecodeError("non-UTF-8 integer".into()))?;
    let n: i64 = num_str
        .parse()
        .map_err(|_| DecodeError(format!("invalid integer '{num_str}'")))?;
    Ok((Value::Integer(n), 1 + end + 1))
}

/// Parse `<len>:<data>` starting at `input[0]` (a digit).
fn decode_bytes(input: &[u8]) -> Result<(Value, usize), DecodeError> {
    let colon = input
        .iter()
        .position(|&b| b == b':')
        .ok_or_else(|| DecodeError("bytes missing ':'".into()))?;
    let len_str = std::str::from_utf8(&input[..colon])
        .map_err(|_| DecodeError("non-UTF-8 length prefix".into()))?;
    let len: usize = len_str
        .parse()
        .map_err(|_| DecodeError(format!("invalid length '{len_str}'")))?;
    let start = colon + 1;
    let end = start + len;
    if end > input.len() {
        return Err(DecodeError(format!(
            "truncated bytes: need {len} bytes but only {} remain",
            input.len() - start
        )));
    }
    Ok((Value::Bytes(input[start..end].to_vec()), end))
}

/// Parse `l<items>e` starting at `input[0]`.
fn decode_list(input: &[u8]) -> Result<(Value, usize), DecodeError> {
    // input[0] == b'l'
    let mut pos = 1;
    let mut items = Vec::new();
    loop {
        if pos >= input.len() {
            return Err(DecodeError("list missing closing 'e'".into()));
        }
        if input[pos] == b'e' {
            return Ok((Value::List(items), pos + 1));
        }
        let (val, consumed) = decode(&input[pos..])?;
        items.push(val);
        pos += consumed;
    }
}

/// Parse `d<pairs>e` starting at `input[0]`.
fn decode_dict(input: &[u8]) -> Result<(Value, usize), DecodeError> {
    // input[0] == b'd'
    let mut pos = 1;
    let mut pairs: Vec<(Vec<u8>, Value)> = Vec::new();
    loop {
        if pos >= input.len() {
            return Err(DecodeError("dict missing closing 'e'".into()));
        }
        if input[pos] == b'e' {
            return Ok((Value::Dict(pairs), pos + 1));
        }
        // Key must be a byte string.
        let (key_val, key_consumed) = decode(&input[pos..])?;
        let key = match key_val {
            Value::Bytes(b) => b,
            other => {
                return Err(DecodeError(format!(
                    "dict key must be a byte string, got {other:?}"
                )));
            }
        };
        pos += key_consumed;
        if pos >= input.len() {
            return Err(DecodeError("dict missing value after key".into()));
        }
        let (val, val_consumed) = decode(&input[pos..])?;
        pos += val_consumed;
        pairs.push((key, val));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- encode helpers ---

    fn roundtrip(v: &Value) {
        let bytes = encode(v);
        let (decoded, consumed) = decode(&bytes).expect("decode failed");
        assert_eq!(&decoded, v);
        assert_eq!(consumed, bytes.len(), "consumed should equal total length");
    }

    // --- basic round-trips ---

    #[test]
    fn roundtrip_bytes() {
        roundtrip(&Value::Bytes(b"hello".to_vec()));
    }

    #[test]
    fn roundtrip_empty_string() {
        roundtrip(&Value::Bytes(b"".to_vec()));
    }

    #[test]
    fn roundtrip_integer_positive() {
        roundtrip(&Value::Integer(42));
    }

    #[test]
    fn roundtrip_integer_zero() {
        roundtrip(&Value::Integer(0));
    }

    #[test]
    fn roundtrip_integer_negative() {
        roundtrip(&Value::Integer(-17));
    }

    #[test]
    fn roundtrip_empty_list() {
        roundtrip(&Value::List(vec![]));
    }

    #[test]
    fn roundtrip_list() {
        roundtrip(&Value::List(vec![
            Value::Integer(1),
            Value::Bytes(b"two".to_vec()),
            Value::Integer(3),
        ]));
    }

    #[test]
    fn roundtrip_empty_dict() {
        roundtrip(&Value::Dict(vec![]));
    }

    #[test]
    fn roundtrip_dict() {
        roundtrip(&Value::dict(vec![
            ("op", Value::string("eval")),
            ("code", Value::string("(+ 1 2)")),
        ]));
    }

    // --- nested structures ---

    #[test]
    fn roundtrip_nested() {
        let v = Value::dict(vec![
            ("op", Value::string("eval")),
            (
                "args",
                Value::List(vec![
                    Value::dict(vec![("x", Value::Integer(1))]),
                    Value::dict(vec![("y", Value::Integer(2))]),
                ]),
            ),
        ]);
        roundtrip(&v);
    }

    // --- nREPL-style message ---

    #[test]
    fn nrepl_eval_message() {
        let msg = Value::dict(vec![
            ("op", Value::string("eval")),
            ("code", Value::string("(->> :post)")),
            ("session", Value::string("abc")),
        ]);
        let bytes = encode(&msg);
        let (decoded, _) = decode(&bytes).expect("decode failed");
        assert_eq!(decoded.get("op").and_then(|v| v.as_str()), Some("eval"));
        assert_eq!(
            decoded.get("code").and_then(|v| v.as_str()),
            Some("(->> :post)")
        );
        assert_eq!(
            decoded.get("session").and_then(|v| v.as_str()),
            Some("abc")
        );
    }

    // --- known wire formats ---

    #[test]
    fn encode_known_bytes() {
        assert_eq!(encode(&Value::Bytes(b"hello".to_vec())), b"5:hello");
    }

    #[test]
    fn encode_known_integer() {
        assert_eq!(encode(&Value::Integer(42)), b"i42e");
    }

    #[test]
    fn encode_known_negative_integer() {
        assert_eq!(encode(&Value::Integer(-3)), b"i-3e");
    }

    #[test]
    fn encode_known_list() {
        assert_eq!(
            encode(&Value::List(vec![
                Value::Integer(1),
                Value::Bytes(b"a".to_vec())
            ])),
            b"li1e1:ae"
        );
    }

    // --- convenience methods ---

    #[test]
    fn as_str_valid_utf8() {
        let v = Value::string("hello");
        assert_eq!(v.as_str(), Some("hello"));
    }

    #[test]
    fn as_str_invalid_utf8() {
        let v = Value::Bytes(vec![0xff, 0xfe]);
        assert_eq!(v.as_str(), None);
    }

    #[test]
    fn get_dict_key() {
        let v = Value::dict(vec![("foo", Value::Integer(99))]);
        assert_eq!(v.get("foo").and_then(|v| v.as_integer()), Some(99));
        assert!(v.get("bar").is_none());
    }

    // --- decode errors ---

    #[test]
    fn decode_error_empty_input() {
        assert!(decode(b"").is_err());
    }

    #[test]
    fn decode_error_truncated_bytes() {
        // "5:hel" — length says 5 but only 3 bytes follow
        assert!(decode(b"5:hel").is_err());
    }

    #[test]
    fn decode_error_truncated_integer() {
        // no closing 'e'
        assert!(decode(b"i42").is_err());
    }

    #[test]
    fn decode_error_truncated_list() {
        // list never closed
        assert!(decode(b"li1e").is_err());
    }

    #[test]
    fn decode_error_truncated_dict() {
        // dict never closed
        assert!(decode(b"d3:foo").is_err());
    }

    #[test]
    fn decode_error_unknown_byte() {
        assert!(decode(b"x").is_err());
    }
}
