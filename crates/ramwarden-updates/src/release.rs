//! Operator-only publisher. Never package this binary or private keys for users.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signer, SigningKey};
use rand::RngCore;
use std::path::Path;
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1)
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match args.first().map(String::as_str){
  Some("keygen") if args.len()==2=>{
   let path=Path::new(&args[1]);let mut bytes=[0;32];rand::rng().fill_bytes(&mut bytes);let key=SigningKey::from_bytes(&bytes);
   let mut options=std::fs::OpenOptions::new();options.write(true).create_new(true);
   #[cfg(unix)]{use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
   use std::io::Write;options.open(path)?.write_all(STANDARD.encode(key.to_bytes()).as_bytes())?;
   std::fs::write(path.with_extension("pub"),STANDARD.encode(key.verifying_key().to_bytes()))?;println!("Created dedicated release key and public-key file; private key was not printed");
  },
  Some("sign") if args.len()==4=>{
   let bytes: [u8;32]=STANDARD.decode(std::fs::read_to_string(&args[1])?.trim())?.try_into().map_err(|_|"Key must be 32 bytes")?;let key=SigningKey::from_bytes(&bytes);let payload=std::fs::read(&args[2])?;
   let envelope=ramwarden_updates::Envelope{signature:STANDARD.encode(key.sign(&payload).to_bytes()),payload:STANDARD.encode(payload)};
   ramwarden_updates::verify(&envelope,&STANDARD.encode(key.verifying_key().to_bytes()),ramwarden_updates::now())?;
   std::fs::write(&args[3],serde_json::to_vec_pretty(&envelope)?)?;println!("Signed update metadata");
  },
  _=>return Err("Use ramwarden-release keygen PRIVATE_PATH or sign PRIVATE_PATH MANIFEST_JSON OUTPUT_JSON".into())
 }
    Ok(())
}
