//! Command-line tooling for signed component catalogs.
//!
//! `keygen` writes a caller-supplied private seed file and prints only the
//! public key. `sign` canonicalizes an unsigned JSON object, signs it, and
//! writes the object plus a lowercase-hex signature. `verify` checks a signed
//! object against a hex public key. The private key is read only from its
//! caller-supplied path and never printed, logged, or copied elsewhere.

use std::fs;
use std::io::Write;
use std::path::Path;

use ed25519_dalek::{Signer, SigningKey};
use oneget_lib::kernel::{canonicalize, verify_manifest};
use rand::RngExt;
use serde_json::{Map, Value};

const SEED_BYTES: usize = 32;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("keygen") => keygen_command(&args[1..]),
        Some("sign") => sign_command(&args[1..]),
        Some("verify") => verify_command(&args[1..]),
        Some(other) => Err(format!("unknown command: {other}")),
        None => Err(String::from(
            "usage: kernel_manifest keygen|sign|verify [options]",
        )),
    };

    if let Err(error) = result {
        eprintln!("kernel_manifest: {error}");
        std::process::exit(1);
    }
}

fn keygen_command(args: &[String]) -> Result<(), String> {
    let private_key_path = required(args, "--private-key")?;
    let mut seed = [0u8; SEED_BYTES];
    rand::rng().fill(&mut seed);

    let signing_key = SigningKey::from_bytes(&seed);
    write_private_key(Path::new(&private_key_path), &seed)?;

    println!("{}", hex::encode(signing_key.verifying_key().to_bytes()));
    Ok(())
}

fn sign_command(args: &[String]) -> Result<(), String> {
    let private_key_path = required(args, "--private-key")?;
    let input_path = required(args, "--input")?;
    let output_path = required(args, "--output")?;

    let signing_key = read_signing_key(&private_key_path)?;
    let bytes = fs::read(&input_path).map_err(|_| String::from("cannot read the input file"))?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| String::from("input is not valid JSON"))?;
    let mut object: Map<String, Value> = match value {
        Value::Object(object) => object,
        _ => return Err(String::from("input must be a JSON object")),
    };
    object.remove("signature");

    let canonical =
        canonicalize(&Value::Object(object.clone())).map_err(|error| error.to_string())?;
    let signature = signing_key.sign(canonical.as_bytes());
    object.insert(
        "signature".to_string(),
        Value::String(hex::encode(signature.to_bytes())),
    );

    let output = serde_json::to_vec(&Value::Object(object))
        .map_err(|_| String::from("signed catalog could not be serialized"))?;
    fs::write(&output_path, output).map_err(|_| String::from("cannot write the output file"))
}

fn verify_command(args: &[String]) -> Result<(), String> {
    let public_key = required(args, "--public-key")?;
    let input_path = required(args, "--input")?;

    let bytes = fs::read(&input_path).map_err(|_| String::from("cannot read the input file"))?;
    verify_manifest(&bytes, &public_key)
        .map_err(|error| format!("verification failed: {}", error.code))?;

    println!("ok");
    Ok(())
}

fn write_private_key(path: &Path, seed: &[u8; SEED_BYTES]) -> Result<(), String> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let mut file = options
        .open(path)
        .map_err(|_| String::from("cannot create the private key file"))?;
    file.write_all(hex::encode(seed).as_bytes())
        .map_err(|_| String::from("cannot write the private key file"))?;
    file.sync_all()
        .map_err(|_| String::from("cannot sync the private key file"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| String::from("cannot restrict the private key file"))?;
    }

    Ok(())
}

fn read_signing_key(path: &str) -> Result<SigningKey, String> {
    let text =
        fs::read_to_string(path).map_err(|_| String::from("cannot read the private key file"))?;
    let decoded =
        hex::decode(text.trim()).map_err(|_| String::from("the private key is not valid hex"))?;
    let seed: [u8; SEED_BYTES] = decoded
        .try_into()
        .map_err(|_| String::from("the private key must be 32 bytes"))?;
    Ok(SigningKey::from_bytes(&seed))
}

fn required(args: &[String], flag: &str) -> Result<String, String> {
    args.iter()
        .position(|argument| argument == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
        .ok_or_else(|| format!("missing required {flag}"))
}
