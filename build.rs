use std::collections::BTreeSet;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

const REVISIONS: [&str; 4] = ["C0", "C1", "C2", "C3"];
const UPSTREAM_COMMIT: &str = "b2a15e9c797b90167d96257a557218ddb7984e71";

fn field<'a>(value: &'a Value, path: &[&str]) -> &'a Value {
    let mut current = value;
    for key in path {
        current = current
            .get(key)
            .unwrap_or_else(|| panic!("manifest is missing {}", path.join(".")));
    }
    current
}

fn expect_string(value: &Value, path: &[&str], expected: &str) {
    assert_eq!(
        field(value, path).as_str(),
        Some(expected),
        "manifest field {} mismatch",
        path.join(".")
    );
}

fn digest(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

fn validate_manifest(revision: &str, manifest: &[u8], bitstream: &[u8]) {
    let value: Value = serde_json::from_slice(manifest)
        .unwrap_or_else(|error| panic!("invalid {revision} manifest JSON: {error}"));
    assert_eq!(field(&value, &["schema_version"]).as_u64(), Some(1));
    expect_string(&value, &["upstream_commit"], UPSTREAM_COMMIT);
    expect_string(&value, &["revision"], revision);
    expect_string(&value, &["profile", "port"], "A");
    expect_string(&value, &["profile", "rx"], "A0");
    expect_string(&value, &["profile", "tx"], "A1");
    assert_eq!(field(&value, &["profile", "voltage"]).as_f64(), Some(3.3));
    assert_eq!(field(&value, &["profile", "data_bits"]).as_u64(), Some(8));
    expect_string(&value, &["profile", "parity"], "none");
    assert_eq!(field(&value, &["profile", "stop_bits"]).as_u64(), Some(1));
    expect_string(&value, &["profile", "flow_control"], "none");
    assert_eq!(field(&value, &["firmware", "api_level"]).as_u64(), Some(9));
    expect_string(&value, &["bitstream", "file"], &format!("{revision}.bit"));
    expect_string(&value, &["bitstream", "sha256"], &digest(bitstream));

    for register in ["baud_divisor", "rx_errors", "rx_overflow", "tx_state"] {
        let address = field(&value, &["registers", register, "address"])
            .as_u64()
            .expect("register address must be an integer");
        assert!(address <= 0x7f, "register address exceeds API-9 range");
        expect_string(&value, &["registers", register, "endian"], "little");
    }
    for direction in ["rx", "tx"] {
        assert_eq!(
            field(&value, &["pipe", direction, "max_packet"]).as_u64(),
            Some(512)
        );
    }
}

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let resources = manifest_dir.join("resources/glasgow-uart");
    assert!(resources.is_dir(), "missing tracked Glasgow UART resources");

    let revisions: BTreeSet<_> = fs::read_dir(&resources)
        .unwrap_or_else(|_| panic!("missing tracked Glasgow UART resources"))
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().and_then(|value| value.to_str()) == Some("json"))
                .then(|| path.file_stem()?.to_str().map(str::to_owned))
                .flatten()
        })
        .filter(|name| name != "build-cache")
        .collect();
    let expected: BTreeSet<_> = REVISIONS.into_iter().map(str::to_owned).collect();
    assert_eq!(
        revisions, expected,
        "expected exactly C0/C1/C2/C3 manifests"
    );

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let mut generated = String::from("pub static GLASGOW_RESOURCES: &[GlasgowResource] = &[\n");
    for revision in REVISIONS {
        let manifest_path = resources.join(format!("{revision}.json"));
        let bitstream_path = resources.join(format!("{revision}.bit"));
        let manifest = fs::read(&manifest_path)
            .unwrap_or_else(|_| panic!("missing tracked {revision} Glasgow UART manifest"));
        let bitstream = fs::read(&bitstream_path)
            .unwrap_or_else(|_| panic!("missing tracked {revision} Glasgow UART bitstream"));
        validate_manifest(revision, &manifest, &bitstream);

        let out_manifest = out_dir.join(format!("{revision}.json"));
        let out_bitstream = out_dir.join(format!("{revision}.bit"));
        fs::write(&out_manifest, manifest).unwrap();
        fs::write(&out_bitstream, bitstream).unwrap();
        writeln!(
            generated,
            "    GlasgowResource {{ revision: \"{revision}\", bitstream: include_bytes!(r\"{}\"), manifest: include_bytes!(r\"{}\") }},",
            out_bitstream.display(),
            out_manifest.display()
        )
        .unwrap();
        println!("cargo:rerun-if-changed={}", manifest_path.display());
        println!("cargo:rerun-if-changed={}", bitstream_path.display());
    }
    generated.push_str("];\n");
    fs::write(out_dir.join("glasgow_resources.rs"), generated).unwrap();
    println!("cargo:rerun-if-changed={}", Path::new("build.rs").display());
}
