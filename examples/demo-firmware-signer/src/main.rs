//! Signs a demo firmware image for the bootloader: fills in the length and
//! the P-256 signature in the image's application header.

use std::{env, fs};

use demo_firmware_core::{meta::firmware, utils::crypto};

fn print_usage(self_name: &String) {
    println!("\nUsage:");
    println!("{self_name} path-to-binary.bin [path-to-privkey.pem]\n");
    println!(concat!(
        "The first argument specifies the file to be signed.\n",
        "The signed file will be created with the same filename but with '.sig' suffix appended.\n",
        "\n",
        "The second argument specifies the path to a private key file in pem format.\n",
        "Note: this argument is optional if the private key is set via\n",
        "the environment variable 'PRIVATEKEYPEM' (data in pem format, not a file path).\n",
    ));
}

fn parse_args() -> Result<(String, String), ()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        println!("Missing input: please specify which file to sign!");
        print_usage(&args[0]);
        return Err(());
    }

    let priv_key = if args.len() >= 3 {
        let path_to_key = &args[2];
        match fs::read_to_string(path_to_key) {
            Ok(success) => success,
            Err(_) => {
                println!("Could not read private key file '{path_to_key}'");
                print_usage(&args[0]);
                return Err(());
            }
        }
    } else {
        match env::var("PRIVATEKEYPEM") {
            Ok(privkey) => privkey,
            Err(_) => {
                println!("Missing input: please specify the private key to use!");
                print_usage(&args[0]);
                return Err(());
            }
        }
    };

    Ok((args[1].clone(), priv_key))
}

fn sign(privkey: String, data: &mut [u8]) -> Result<(), &'static str> {
    println!("{} bytes total", data.len());

    let key = crypto::PrivKey::try_from_pem(privkey.as_str()).map_err(|error| {
        println!("Private key error: {error:?}");
        "Private key could not be parsed"
    })?;

    firmware::sign(&key, data).map_err(|error| {
        println!("Failed to create signature: {error:?}");
        "Failed to create signature"
    })?;
    Ok(())
}

fn main() -> Result<(), &'static str> {
    let (bin_file, privkey) = parse_args().map_err(|_| "Failed to parse args")?;

    println!("Signing binary file '{bin_file}'...");
    let mut bin_data = fs::read(&bin_file).map_err(|_| "Failed to read binary")?;

    sign(privkey, bin_data.as_mut_slice()).map_err(|_| "Failed to sign binary")?;

    let mut signed_file = bin_file.clone();
    signed_file.push_str(".sig");
    fs::write(&signed_file, bin_data).map_err(|_| "Failed to write signed binary")?;

    println!("OK: signed binary as '{signed_file}'");

    Ok(())
}
