use std::io;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("protocol I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("frame encoding failed: {0}")]
    Encode(#[from] bincode::error::EncodeError),
    #[error("frame decoding failed: {0}")]
    Decode(#[from] bincode::error::DecodeError),
    #[error("frame exceeds limit")]
    FrameLimit,
    #[error("empty frame")]
    EmptyFrame,
    #[error("clipboard image must contain 1 byte through 16 MiB")]
    ClipboardImageSize,
    #[error("unsupported clipboard image extension")]
    ClipboardImageExtension,
    #[error("clipboard image target must contain 1 through 1024 bytes")]
    ClipboardImageTarget,
    #[error("trailing frame bytes")]
    TrailingBytes,
    #[error("cell count does not match frame dimensions")]
    CellCount,
    #[error("invalid hyperlink index")]
    HyperlinkIndex,
    #[error("cursor outside frame")]
    CursorBounds,
    #[error("surface patch identity mismatch")]
    PatchIdentity,
    #[error("surface patch row outside frame")]
    PatchRowBounds,
    #[error("surface patch changed pane geometry")]
    PatchGeometry,
    #[error("patch cursor outside frame")]
    PatchCursorBounds,
}

impl Error {
    /// I/O failures retain their original retry category; malformed data does not retry.
    pub fn kind(&self) -> io::ErrorKind {
        match self {
            Self::Io(error) => error.kind(),
            _ => io::ErrorKind::InvalidData,
        }
    }
}
