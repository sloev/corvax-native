//! Bencode (BEP 3), as the JS core encodes it: dictionaries sorted by key bytes.
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Value { Int(i64), Bytes(Vec<u8>), List(Vec<Value>), Dict(BTreeMap<Vec<u8>, Value>) }

impl Value {
    pub fn bytes(&self) -> Option<&[u8]> { if let Value::Bytes(b) = self { Some(b) } else { None } }
    pub fn int(&self) -> Option<i64> { if let Value::Int(i) = self { Some(*i) } else { None } }
    pub fn list(&self) -> Option<&[Value]> { if let Value::List(l) = self { Some(l) } else { None } }
    pub fn dict(&self) -> Option<&BTreeMap<Vec<u8>, Value>> { if let Value::Dict(d) = self { Some(d) } else { None } }
    pub fn get(&self, k: &str) -> Option<&Value> { self.dict()?.get(k.as_bytes()) }
    pub fn str(&self) -> Option<&str> { std::str::from_utf8(self.bytes()?).ok() }
}
impl From<i64> for Value { fn from(i: i64) -> Self { Value::Int(i) } }
impl From<&[u8]> for Value { fn from(b: &[u8]) -> Self { Value::Bytes(b.to_vec()) } }
impl From<Vec<u8>> for Value { fn from(b: Vec<u8>) -> Self { Value::Bytes(b) } }
impl From<&str> for Value { fn from(s: &str) -> Self { Value::Bytes(s.as_bytes().to_vec()) } }

/// A dictionary from (key, value) pairs.
pub fn dict<const N: usize>(items: [(&str, Value); N]) -> Value {
    Value::Dict(items.into_iter().map(|(k, v)| (k.as_bytes().to_vec(), v)).collect())
}

pub fn encode(v: &Value) -> Vec<u8> { let mut out = Vec::new(); put(v, &mut out); out }
fn put(v: &Value, out: &mut Vec<u8>) {
    match v {
        Value::Int(i) => out.extend(format!("i{i}e").bytes()),
        Value::Bytes(b) => { out.extend(format!("{}:", b.len()).bytes()); out.extend(b) }
        Value::List(l) => { out.push(b'l'); l.iter().for_each(|x| put(x, out)); out.push(b'e') }
        Value::Dict(d) => { out.push(b'd'); for (k, x) in d { put(&Value::Bytes(k.clone()), out); put(x, out) } out.push(b'e') }
    }
}

pub fn decode(b: &[u8]) -> Option<Value> {
    let (v, n) = take(b, 0, 0)?;
    (n == b.len()).then_some(v)
}
fn take(b: &[u8], i: usize, depth: u8) -> Option<(Value, usize)> {
    if depth > 32 { return None }
    match *b.get(i)? {
        b'i' => { let e = i + b[i..].iter().position(|&c| c == b'e')?; Some((Value::Int(std::str::from_utf8(&b[i + 1..e]).ok()?.parse().ok()?), e + 1)) }
        b'l' => { let (mut l, mut j) = (Vec::new(), i + 1); while *b.get(j)? != b'e' { let (v, n) = take(b, j, depth + 1)?; l.push(v); j = n } Some((Value::List(l), j + 1)) }
        b'd' => {
            let (mut d, mut j) = (BTreeMap::new(), i + 1);
            while *b.get(j)? != b'e' { let (k, n) = take(b, j, depth + 1)?; let (v, m) = take(b, n, depth + 1)?; d.insert(k.bytes()?.to_vec(), v); j = m }
            Some((Value::Dict(d), j + 1))
        }
        b'0'..=b'9' => {
            let c = i + b[i..].iter().position(|&c| c == b':')?;
            let n: usize = std::str::from_utf8(&b[i..c]).ok()?.parse().ok()?;
            Some((Value::Bytes(b.get(c + 1..c + 1 + n)?.to_vec()), c + 1 + n))
        }
        _ => None,
    }
}
