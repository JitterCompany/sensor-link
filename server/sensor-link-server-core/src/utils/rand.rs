use rand::random;

pub fn rand_string(size: usize) -> String {
    (0..)
        .map(|_| random::<char>())
        .filter(|c| c.is_ascii())
        .take(size)
        .collect()
}

pub fn rand_alphanumeric(size: usize) -> String {
    (0..)
        .map(|_| random::<char>())
        .filter(|c| c.is_ascii_alphanumeric())
        .take(size)
        .collect()
}
