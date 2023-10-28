
#[derive(Debug, thiserror::Error)]
pub enum MainError {

    #[error("{0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    IpfsApi(#[from] ipfs_api::Error),

    #[error("{0}")]
    Message(String),

}
