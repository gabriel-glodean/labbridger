/// Small CLI utility – prints the bcrypt hash of a password so you can
/// paste it into config.yaml.
///
/// Usage:
///   cargo run --bin hash-password -- mysecretpassword
fn main() {
    let password = std::env::args()
        .nth(1)
        .expect("Usage: hash-password <password>");

    let hash = bcrypt::hash(&password, bcrypt::DEFAULT_COST)
        .expect("bcrypt hashing failed");

    println!("{}", hash);
}

