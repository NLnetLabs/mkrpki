//! Making of RPKI-related objects.

use std::io::{Read, Write};
use std::fmt::Write as _;
use std::ffi::OsStr;
use std::fs::File;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use chrono::Duration;
use rpki::cert::{KeyUsage, Overclaim, TbsCert};
use rpki::crl::{TbsCertList, CrlEntry};
use rpki::crypto::{DigestAlgorithm, PublicKey, SignatureAlgorithm, Signer};
use rpki::crypto::softsigner::{OpenSslSigner, KeyId};
use rpki::manifest::{FileAndHash, ManifestContent};
use rpki::roa::{RoaBuilder, RoaIpAddress};
use rpki::resources::{AsBlock, AsId, IpBlock};
use rpki::sigobj::SignedObjectBuilder;
use rpki::x509::{Serial, Time, Validity};
use rpki::uri;
use structopt::StructOpt;
use unwrap::unwrap;


//------------ main ----------------------------------------------------------

fn main() {
    if let Err(()) = Operation::from_args().run() {
        std::process::exit(1)
    }
}


//------------ Operation -----------------------------------------------------

#[derive(StructOpt)]
#[structopt(name="mkrpki", about="Creates RPKI objects.")]
#[allow(clippy::large_enum_variant)]
enum Operation {
    /// Creates a key pair.
    #[structopt(name="key")]
    Key(Key),

    /// Creates a trust-anchor certificate.
    #[structopt(name="ta")]
    Ta(Ta),

    /// Creates a CA certificate.
    #[structopt(name="cer")]
    Cert(Cert),

    /// Creates a CRL.
    #[structopt(name="crl")]
    Crl(Crl),

    /// Creates a ROA.
    #[structopt(name="roa")]
    Roa(Roa),

    /// Creates a manifest.
    #[structopt(name="mft")]
    Mft(Mft),
}

impl Operation {
    pub fn run(self) -> Result<(), ()> {
        match self {
            Operation::Key(key) => key.run(),
            Operation::Ta(ta) => ta.run(),
            Operation::Cert(cert) => cert.run(),
            Operation::Crl(crl) => crl.run(),
            Operation::Roa(roa) => roa.run(),
            Operation::Mft(mft) => mft.run(),
        }
    }
}


//------------ Key -----------------------------------------------------------

#[derive(StructOpt)]
struct Key {
    /// The path to the private key file.
    #[structopt(long = "private")]
    private: PathBuf,
    
    /// The path to the public key file.
    #[structopt(long = "public")]
    public: PathBuf,
}

impl Key {
    pub fn run(self) -> Result<(), ()> {
        let key = match openssl::rsa::Rsa::generate(2048) {
            Ok(key) => key,
            Err(err) => {
                eprintln!("Failed to generate key: {}", err);
                return Err(())
            }
        };

        let mut file = match File::create(&self.private) {
            Ok(file) => file,
            Err(err) => {
                eprintln!("Failed to open private key file: {}", err);
                return Err(())
            }
        };
        let buf = match key.private_key_to_der() {
            Ok(buf) => buf,
            Err(err) => {
                eprintln!("Failed to extract private key: {}", err);
                return Err(())
            }
        };
        if let Err(err) = file.write_all(&buf) {
            eprintln!("Failed to write to private key file: {}", err);
            return Err(())
        }

        let mut file = match File::create(&self.public) {
            Ok(file) => file,
            Err(err) => {
                eprintln!("Failed to open public key file: {}", err);
                return Err(())
            }
        };
        let buf = match key.public_key_to_der() {
            Ok(buf) => buf,
            Err(err) => {
                eprintln!("Failed to extract public key: {}", err);
                return Err(())
            }
        };
        if let Err(err) = file.write_all(&buf) {
            eprintln!("Failed to write to public key file: {}", err);
            return Err(())
        }

        eprintln!("key: {}", self.private.display());
        eprintln!("pub:  {}", self.public.display());
        Ok(())
    }
}


//------------ Ta ------------------------------------------------------------

#[derive(StructOpt)]
struct Ta {
    /// Path to the private key of the certificate.
    #[structopt(long="key")]
    key: PathBuf,

    /// Serial number of the certificate.
    #[structopt(long="serial")]
    serial: Serial,

    /// Not-before date of the certificate. Defaults to now.
    #[structopt(long="not-before")]
    not_before: Option<Time>,

    /// Not-after date of the certificate.
    #[structopt(long="not-after")]
    not_after: Option<Time>,

    /// Duration of validity of certificate in days.
    #[structopt(long="days")]
    valid_days: Option<i64>,

    /// CA repository URI.
    #[structopt(long="ca-repository")]
    ca_repository: uri::Rsync,

    /// RPKI manifest URI.
    #[structopt(long="rpki-manifest")]
    rpki_manifest: uri::Rsync,

    /// Optional RPKI notify URI.
    #[structopt(long="rpki-notify")]
    rpki_notify: Option<uri::Https>,

    /// IPv4 resources.
    #[structopt(long="v4")]
    v4_resources: Vec<IpBlock>,

    /// IPv4 resources.
    #[structopt(long="v6")]
    v6_resources: Vec<IpBlock>,

    /// AS resources.
    #[structopt(long="as")]
    as_resources: Vec<AsBlock>,

    /// Rsync URI to include in the TAL file.
    #[structopt(long="tal-rsync-uri")]
    tal_rsync_uri: uri::Rsync,

    /// Optional HTTPS URI to include in the TAL file.
    #[structopt(long="tal-https-uri")]
    tal_https_uri: Option<uri::Https>,

    /// Path to file to write the certificate into.
    #[structopt(long="output")]
    output_ta: PathBuf,

    /// Path to file to write the TAL into.
    #[structopt(long="output-tal")]
    output_tal: Option<PathBuf>,
}

impl Ta {
    pub fn run(self) -> Result<(), ()> {
        let (signer, key) = create_signer(&self.key)?;
        let key_pub = unwrap!(signer.get_key_info(&key));

        let not_before = self.not_before.unwrap_or_else(Time::now);
        let validity = if let Some(not_after) = self.not_after {
            Validity::new(not_before, not_after)
        }
        else if let Some(valid_days) = self.valid_days {
            Validity::new(not_before, not_before + Duration::days(valid_days))
        }
        else {
            eprintln!("Either --not-after or --days must be given.");
            return Err(())
        };

        let mut cert = TbsCert::new(
            self.serial,
            key_pub.to_subject_name(),
            validity,
            None,
            key_pub.clone(),
            KeyUsage::Ca,
            Overclaim::Refuse,
        );
        cert.set_basic_ca(Some(true));
        cert.set_authority_key_identifier(Some(key_pub.key_identifier()));
        cert.set_ca_repository(Some(self.ca_repository));
        cert.set_rpki_manifest(Some(self.rpki_manifest));
        if let Some(rpki_notify) = self.rpki_notify {
            cert.set_rpki_notify(Some(rpki_notify));
        }
        if !self.v4_resources.is_empty() {
            cert.v4_resources_from_iter(self.v4_resources);
        }
        if !self.v6_resources.is_empty() {
            cert.v6_resources_from_iter(self.v6_resources);
        }
        if !self.as_resources.is_empty() {
            cert.as_resources_from_iter(self.as_resources);
        }

        let cert = unwrap!(cert.into_cert(&signer, &key)).to_captured();
        save_file(&self.output_ta, &cert)?;
        eprintln!("TA:  {}", self.output_ta.display());
        
        if let Some(path) = self.output_tal {
            let mut tal = format!("{}\n", self.tal_rsync_uri);
            if let Some(uri) = self.tal_https_uri {
                unwrap!(writeln!(tal, "{}", uri));
            }
            unwrap!(writeln!(tal, ""));
            unwrap!(
                writeln!(tal, "{}", base64::encode(&key_pub.to_info_bytes()))
            );
            save_file(&path, tal.as_bytes())?;
            eprintln!("TAL: {}", path.display());
        }
        Ok(())
    }
}


//------------ Cert ----------------------------------------------------------

#[derive(StructOpt)]
struct Cert {
    /// Path to the private key of the certificate issuer.
    #[structopt(long="issuer-key")]
    issuer_key: PathBuf,

    /// Path to the public key of the certificate subject.
    #[structopt(long="subject-key")]
    subject_key: PathBuf,

    /// Serial number of the certificate.
    #[structopt(long="serial")]
    serial: Serial,

    /// Not-before date of the certificate. Defaults to now.
    #[structopt(long="not-before")]
    not_before: Option<Time>,

    /// Not-after date of the certificate.
    #[structopt(long="not-after")]
    not_after: Option<Time>,

    /// Duration of validity of certificate in days.
    #[structopt(long="days")]
    valid_days: Option<i64>,

    /// Overclaiming resources should be trimmed.
    #[structopt(long="trim-resources")]
    trim_resources: bool,

    /// RPKI URI of the CRL.
    #[structopt(long="crl")]
    crl_uri: uri::Rsync,

    /// CA issuer URI.
    #[structopt(long="ca-issuer")]
    ca_issuer: uri::Rsync,

    /// CA repository URI.
    #[structopt(long="ca-repository")]
    ca_repository: uri::Rsync,

    /// RPKI manifest URI.
    #[structopt(long="rpki-manifest")]
    rpki_manifest: uri::Rsync,

    /// Optional RPKI notify URI.
    #[structopt(long="rpki-notify")]
    rpki_notify: Option<uri::Https>,

    /// IPv4 resources.
    #[structopt(long="v4")]
    v4_resources: Vec<IpBlock>,

    /// Inherit IPv4 resources. Overides any explicit resources.
    #[structopt(long="inherit-v4")]
    inherit_v4: bool,

    /// IPv4 resources.
    #[structopt(long="v6")]
    v6_resources: Vec<IpBlock>,

    /// Inherit IPv4 resources. Overides any explicit resources.
    #[structopt(long="inherit-v6")]
    inherit_v6: bool,

    /// AS resources.
    #[structopt(long="as")]
    as_resources: Vec<AsBlock>,

    /// Inherit AS resources. Overides any explicit resources.
    #[structopt(long="inherit-as")]
    inherit_as: bool,

    /// Path to file to write the certificate into.
    #[structopt(long="output")]
    output: PathBuf
}

impl Cert {
    pub fn run(self) -> Result<(), ()> {
        let (signer, issuer_key) = create_signer(&self.issuer_key)?;
        let issuer_pub = unwrap!(signer.get_key_info(&issuer_key));
        let subject_key = load_file(&self.subject_key)?;
        let subject_key = match PublicKey::decode(subject_key.as_slice()) {
            Ok(key) => key,
            Err(err) => {
                eprintln!("Failed to load subject public key: {}", err);
                return Err(())
            }
        };

        let not_before = self.not_before.unwrap_or_else(Time::now);
        let validity = if let Some(not_after) = self.not_after {
            Validity::new(not_before, not_after)
        }
        else if let Some(valid_days) = self.valid_days {
            Validity::new(not_before, not_before + Duration::days(valid_days))
        }
        else {
            eprintln!("Either --not-after or --days must be given.");
            return Err(())
        };

        let mut cert = TbsCert::new(
            self.serial,
            issuer_pub.to_subject_name(),
            validity,
            None,
            subject_key,
            KeyUsage::Ca,
            if self.trim_resources { Overclaim::Trim }
            else { Overclaim::Refuse }
        );
        cert.set_basic_ca(Some(true));
        cert.set_authority_key_identifier(Some(issuer_pub.key_identifier()));
        cert.set_crl_uri(Some(self.crl_uri));
        cert.set_ca_issuer(Some(self.ca_issuer));
        cert.set_ca_repository(Some(self.ca_repository));
        cert.set_rpki_manifest(Some(self.rpki_manifest));
        if let Some(rpki_notify) = self.rpki_notify {
            cert.set_rpki_notify(Some(rpki_notify));
        }
        if self.inherit_v4 {
            cert.set_v4_resources_inherit()
        }
        else if !self.v4_resources.is_empty() {
            cert.v4_resources_from_iter(self.v4_resources)
        }
        if self.inherit_v6 {
            cert.set_v6_resources_inherit()
        }
        else if !self.v6_resources.is_empty() {
            cert.v6_resources_from_iter(self.v6_resources)
        }
        if self.inherit_as {
            cert.set_as_resources_inherit()
        }
        else if !self.as_resources.is_empty() {
            cert.as_resources_from_iter(self.as_resources)
        }

        let cert = unwrap!(cert.into_cert(&signer, &issuer_key)).to_captured();
        save_file(&self.output, &cert)?;
        eprintln!("Cer: {}", self.output.display());
        Ok(())
    }
}


//------------ Crl -----------------------------------------------------------

#[derive(StructOpt)]
struct Crl {
    /// Path to the private key of the certificate issuer.
    #[structopt(long="issuer-key")]
    issuer_key: PathBuf,

    /// Time of this update. Defaults to now.
    #[structopt(long = "this-update")]
    this_update: Option<Time>,

    /// Time of the next update.
    #[structopt(long = "next-update")]
    next_update: Option<Time>,

    /// The number of days until the next update.
    #[structopt(long="next-days")]
    next_days: Option<i64>,

    /// Revoked certificates.
    #[structopt(short = "c", long = "cert")]
    revoked_certs: Vec<CrlEntry>,

    /// CRL number.
    #[structopt(long = "crl")]
    crl_number: Serial,

    /// Path to file to write the CRL into.
    #[structopt(long="output")]
    output: PathBuf
}

impl Crl {
    pub fn run(self) -> Result<(), ()> {
        let (signer, issuer_key) = create_signer(&self.issuer_key)?;
        let issuer_pub = unwrap!(signer.get_key_info(&issuer_key));

        let this_update = self.this_update.unwrap_or_else(Time::now);
        let next_update = if let Some(next_update) = self.next_update {
            next_update
        }
        else if let Some(days) = self.next_days {
            this_update + Duration::days(days)
        }
        else {
            eprintln!("Either --next-update or --next-days must be given.");
            return Err(())
        };

        let crl = TbsCertList::new(
            SignatureAlgorithm::default(),
            issuer_pub.to_subject_name(),
            this_update,
            next_update,
            self.revoked_certs,
            issuer_pub.key_identifier(),
            self.crl_number
        );

        let crl = unwrap!(crl.into_crl(&signer, &issuer_key)).to_captured();
        save_file(&self.output, &crl)?;
        eprintln!("Crl: {}", self.output.display());
        Ok(())
    }
}


//------------ Roa -----------------------------------------------------------

#[derive(StructOpt)]
struct Roa {
    /// Path to the private key of the certificate issuer.
    #[structopt(long="issuer-key")]
    issuer_key: PathBuf,

    /// Serial number of the certificate.
    #[structopt(long="serial")]
    serial: Serial,

    /// Not-before date of the certificate. Defaults to now.
    #[structopt(long="not-before")]
    not_before: Option<Time>,

    /// Not-after date of the certificate.
    #[structopt(long="not-after")]
    not_after: Option<Time>,

    /// Duration of validity of certificate in days.
    #[structopt(long="days")]
    valid_days: Option<i64>,

    /// RPKI URI of the CRL.
    #[structopt(long="crl")]
    crl_uri: uri::Rsync,

    /// CA issuer URI.
    #[structopt(long="ca-issuer")]
    ca_issuer: uri::Rsync,

    /// Signed Object URI
    #[structopt(long="signed-object")]
    signed_object: uri::Rsync,

    /// The AS number for the ROA.
    #[structopt(long="asn")]
    asn: AsId,

    /// The IP prefixes for the ROA.
    #[structopt(long="prefixes")]
    prefixes: Vec<RoaPrefix>,

    /// Path to file to write the certificate into.
    #[structopt(long="output")]
    output: PathBuf
}

impl Roa {
    pub fn run(self) -> Result<(), ()> {
        let (mut v4, mut v6) = (Vec::new(), Vec::new());
        for prefix in self.prefixes {
            if prefix.v4 {
                v4.push(prefix.prefix)
            }
            else {
                v6.push(prefix.prefix)
            }
        }
        let (signer, issuer_key) = create_signer(&self.issuer_key)?;

        let not_before = self.not_before.unwrap_or_else(Time::now);
        let validity = if let Some(not_after) = self.not_after {
            Validity::new(not_before, not_after)
        }
        else if let Some(valid_days) = self.valid_days {
            Validity::new(not_before, not_before + Duration::days(valid_days))
        }
        else {
            eprintln!("Either --not-after or --days must be given.");
            return Err(())
        };

        let mut roa = RoaBuilder::new(self.asn);
        roa.extend_v4_from_slice(&v4);
        roa.extend_v6_from_slice(&v6);

        let roa = unwrap!(roa.finalize(
            SignedObjectBuilder::new(
                self.serial, validity, self.crl_uri, self.ca_issuer,
                self.signed_object
            ),
            &signer, &issuer_key
        ));
        let roa = roa.to_captured();
        save_file(&self.output, &roa)?;
        eprintln!("Roa: {}", self.output.display());
        Ok(())
    }
}


//------------ RoaPrefix -----------------------------------------------------

#[derive(Clone, Debug)]
struct RoaPrefix {
    v4: bool,
    prefix: RoaIpAddress,
}

impl FromStr for RoaPrefix {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (prefix, maxlen) = match s.find('-') {
            Some(idx) => (&s[..idx], Some(&s[idx + 1..])),
            None => (s, None)
        };
        let (addr, len) = match prefix.find('/') {
            Some(idx) => (&prefix[..idx], &prefix[idx + 1..]),
            None => return Err(format!("Invalid ROA prefix '{}'", s))
        };
        let addr = match IpAddr::from_str(addr) {
            Ok(addr) => addr,
            Err(_) => return Err(format!("Invalid ROA prefix '{}'", s))
        };
        let len = match u8::from_str(len) {
            Ok(len) => len,
            Err(_) => return Err(format!("Invalid ROA prefix '{}'", s))
        };
        let maxlen = match maxlen {
            Some(maxlen) => match u8::from_str(maxlen) {
                Ok(maxlen) => Some(maxlen),
                Err(_) => return Err(format!("Invalid ROA prefix '{}'", s))
            },
            None => None
        };
        Ok(RoaPrefix {
            v4: addr.is_ipv4(),
            prefix: RoaIpAddress::new_addr(addr, len, maxlen)
        })
    }
}


//------------ Mft -----------------------------------------------------------

#[derive(StructOpt)]
struct Mft {
    /// Path to the private key of the certificate issuer.
    #[structopt(long="issuer-key")]
    issuer_key: PathBuf,

    /// Serial number of the certificate.
    #[structopt(long="serial")]
    serial: Serial,

    /// Not-before date of the certificate. Defaults to now.
    #[structopt(long="not-before")]
    not_before: Option<Time>,

    /// Not-after date of the certificate.
    #[structopt(long="not-after")]
    not_after: Option<Time>,

    /// Duration of validity of certificate in days.
    #[structopt(long="days")]
    valid_days: Option<i64>,

    /// RPKI URI of the CRL.
    #[structopt(long="crl")]
    crl_uri: uri::Rsync,

    /// CA issuer URI.
    #[structopt(long="ca-issuer")]
    ca_issuer: uri::Rsync,

    /// The number of this manifest.
    #[structopt(long="number")]
    number: Serial,

    /// Signed Object URI
    #[structopt(long="signed-object")]
    signed_object: uri::Rsync,

    /// The update time of this manifest.
    #[structopt(long="this-update")]
    this_update: Option<Time>,

    /// The update time of the next manifest.
    #[structopt(long="next-update")]
    next_update: Option<Time>,

    /// The number of days until the next update.
    #[structopt(long="next-days")]
    next_days: Option<i64>,

    /// The files to include in the manifest
    #[structopt(long="files")]
    files: Vec<PathBuf>,

    /// Path to file to write the certificate into.
    #[structopt(long="output")]
    output: PathBuf,
}

impl Mft {
    pub fn run(self) -> Result<(), ()> {
        let (signer, issuer_key) = create_signer(&self.issuer_key)?;

        let not_before = self.not_before.unwrap_or_else(Time::now);
        let validity = if let Some(not_after) = self.not_after {
            Validity::new(not_before, not_after)
        }
        else if let Some(valid_days) = self.valid_days {
            Validity::new(not_before, not_before + Duration::days(valid_days))
        }
        else {
            eprintln!("Either --not-after or --days must be given.");
            return Err(())
        };
        let this_update = self.this_update.unwrap_or_else(Time::now);
        let next_update = if let Some(next_update) = self.next_update {
            next_update
        }
        else if let Some(days) = self.next_days {
            this_update + Duration::days(days)
        }
        else {
            eprintln!("Either --next-update or --next-days must be given.");
            return Err(())
        };

        let alg = DigestAlgorithm::default();
        let mut files = Vec::new();
        for path in self.files {
            let mut file = match File::open(&path) {
                Ok(file) => file,
                Err(err) => {
                    eprintln!("Cannot open file {}: {}", path.display(), err);
                    return Err(())
                }
            };
            let name = match path.file_name().and_then(OsStr::to_str) {
                Some(name) if name.is_ascii() => name.to_string(),
                _ => {
                    eprintln!("Illegal file name {}.", path.display());
                    return Err(())
                }
            };
            let mut digest = alg.start();
            let mut buf = [0u8; 4096];
            loop {
                let read = match file.read(&mut buf) {
                    Ok(read) => read,
                    Err(err) => {
                        eprintln!(
                            "Cannot read file {}: {}", path.display(), err
                        );
                        return Err(())
                    }
                };
                if read == 0 {
                    break;
                }
                digest.update(&buf[..read]);
            }
            files.push(FileAndHash::new(name, digest.finish()));
        }

        let content = ManifestContent::new(
            self.number, this_update, next_update, alg, files
        );

        let manifest = unwrap!(content.into_manifest(
            SignedObjectBuilder::new(
                self.serial, validity, self.crl_uri, self.ca_issuer,
                self.signed_object
            ),
            &signer, &issuer_key
        ));
        let manifest = manifest.to_captured();
        save_file(&self.output, &manifest)?;
        eprintln!("Mft: {}", self.output.display());
        Ok(())
    }
}


//------------ Helpers -------------------------------------------------------

fn create_signer(issuer_key: &Path) -> Result<(OpenSslSigner, KeyId), ()> {
    let mut signer = OpenSslSigner::new();
    let der = load_file(issuer_key)?;
    let key = match signer.key_from_der(&der) {
        Ok(key) => key,
        Err(err) => {
            eprintln!(
                "Invalid issuer key {}: {}",
                issuer_key.display(), err
            );
            return Err(())
        }
    };
    Ok((signer, key))
}

fn load_file(path: &Path) -> Result<Vec<u8>, ()> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(err) => {
            eprintln!("Failed to open file {}: {}", path.display(), err);
            return Err(())
        }
    };
    let mut res = Vec::new();
    if let Err(err) = file.read_to_end(&mut res) {
        eprintln!(
            "Failed to read file {}: {}",
            path.display(), err
        );
        return Err(())
    }
    Ok(res)
}

fn save_file(path: &Path, content: &[u8]) -> Result<(), ()> {
    let mut file = match File::create(path) {
        Ok(file) => file,
        Err(err) => {
            eprintln!("Failed to open file {}: {}", path.display(), err);
            return Err(())
        }
    };
    if let Err(err) = file.write_all(content) {
        eprintln!("Failed to write to file {}: {}", path.display(), err);
        Err(())
    }
    else {
        Ok(())
    }
}

