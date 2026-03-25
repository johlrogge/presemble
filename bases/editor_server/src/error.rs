#[derive(Debug)]
pub enum ServerError {}

impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "server error")
    }
}

impl std::error::Error for ServerError {}
