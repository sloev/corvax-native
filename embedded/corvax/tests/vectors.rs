//! Conformance with the JS core: every vector in vectors.json (made by CORVAX's scripts/vectors.js).
use corvax::bencode::{decode, encode, Value};
use corvax::crypto::{hkdf, open, seal, verify, Identity};
use corvax::record::{signable, Anchor, Beacon};
use corvax::text::{hex, unhex, unz32, z32};
use serde_json::Value as J;

fn vectors() -> J { serde_json::from_str(include_str!("vectors.json")).unwrap() }
fn seed(s: &J) -> [u8; 32] { unhex(s.as_str().unwrap()).unwrap().try_into().unwrap() }
fn from_json(v: &J) -> Value {
    match v {
        J::Number(n) => Value::Int(n.as_i64().unwrap()),
        J::String(s) => s.as_str().into(),
        J::Array(a) => Value::List(a.iter().map(from_json).collect()),
        J::Object(o) => Value::Dict(o.iter().map(|(k, v)| (k.as_bytes().to_vec(), from_json(v))).collect()),
        _ => unreachable!(),
    }
}

#[test]
fn z32_and_bencode() {
    let v = vectors();
    for x in v["z32"].as_array().unwrap() {
        let b = unhex(x["bytes"].as_str().unwrap()).unwrap();
        assert_eq!(z32(&b), x["z32"].as_str().unwrap());
        assert_eq!(unz32(x["z32"].as_str().unwrap()).unwrap(), b);
    }
    for x in v["bencode"].as_array().unwrap() {
        let bytes = encode(&from_json(&x["value"]));
        assert_eq!(hex(&bytes), x["hex"].as_str().unwrap());
        assert_eq!(decode(&bytes).unwrap(), from_json(&x["value"]));
    }
}

#[test]
fn identity_hkdf_and_signatures() {
    let v = vectors();
    let h = &v["hkdf"][0];
    assert_eq!(hex(&hkdf(&unhex(h["ikm"].as_str().unwrap()).unwrap(), h["info"].as_str().unwrap().as_bytes())), h["okm"].as_str().unwrap());
    for x in v["identity"].as_array().unwrap() {
        let id = Identity::new(seed(&x["seed"]));
        assert_eq!(hex(&id.ed_pub), x["ed_pub"].as_str().unwrap());
        assert_eq!(hex(&id.x_pub), x["x_pub"].as_str().unwrap());
        assert_eq!(id.address(), x["address"].as_str().unwrap());
    }
    for x in v["sign"].as_array().unwrap() {
        let id = Identity::new(seed(&x["seed"]));
        let msg = unhex(x["msg"].as_str().unwrap()).unwrap();
        assert_eq!(hex(&id.sign(&msg)), x["sig"].as_str().unwrap(), "Ed25519 is deterministic: same bytes as WebCrypto");
        assert!(verify(&id.ed_pub, &unhex(x["sig"].as_str().unwrap()).unwrap(), &msg));
    }
}

#[test]
fn sealed_messages_from_js_open_here_and_back() {
    let v = vectors();
    let now = v["seal_valid_until"].as_u64().unwrap() - 400_000;
    for x in v["seal"].as_array().unwrap() {
        let to = Identity::new(seed(&x["to_seed"]));
        let from = Identity::new(seed(&x["from_seed"]));
        let m = open(&to, &unhex(x["wire"].as_str().unwrap()).unwrap(), now).unwrap_or_else(|| panic!("open: {}", x["note"]));
        assert_eq!(m.from, from.ed_pub);
        assert_eq!(m.body, x["body"]);
        if let Some(e) = x.get("exp") { assert_eq!(m.exp, e.as_u64().unwrap()) }
        // and the other way: sealed here, opened here (the JS side opens Rust's in the interop test)
        let back = seal(&to, &from.ed_pub, &from.x_pub, &x["body"], true, now, None).unwrap();
        assert_eq!(open(&from, &back, now).unwrap().body, x["body"]);
        assert!(open(&from, &back, now + 700_000).is_none(), "10-minute window");
    }
}

#[test]
fn beacons_and_anchor_lines() {
    let v = vectors();
    let b = &v["beacon"][0];
    let id = Identity::new(seed(&b["seed"]));
    let fields: Vec<(&str, String)> = b["fields"].as_array().unwrap().iter().filter(|f| f[0] != "x").map(|f| (f[0].as_str().unwrap(), f[1].as_str().unwrap().to_string())).collect();
    let mine = Beacon::make(&id, &fields, b["seq"].as_u64().unwrap()).unwrap();
    assert_eq!(hex(&mine.v), b["v"].as_str().unwrap());
    assert_eq!(hex(&signable(mine.seq, &mine.v)), b["signable"].as_str().unwrap());
    assert_eq!(hex(&mine.sig), b["sig"].as_str().unwrap());
    assert_eq!(hex(&mine.pack()), b["packed"].as_str().unwrap());
    let back = Beacon::unpack(id.ed_pub, &unhex(b["packed"].as_str().unwrap()).unwrap()).unwrap();
    assert!(back.valid());
    assert_eq!(back.field("n"), ["esp32"]);
    assert_eq!(back.version(), 2);
    assert_eq!(back.x_pub().unwrap(), id.x_pub);
    for x in v["anchor_line"].as_array().unwrap() {
        let a = Anchor::parse(x["line"].as_str().unwrap()).unwrap();
        assert_eq!(a.key, x["parsed"]["key"].as_str().unwrap());
        assert_eq!(a.host, x["parsed"]["host"].as_str().unwrap());
        assert_eq!(a.port as u64, x["parsed"]["port"].as_u64().unwrap());
        assert_eq!(a.line(), x["formatted"].as_str().unwrap().split(";q=").next().unwrap());
    }
}
