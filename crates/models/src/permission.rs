/// Canonical permission names matching Python's `PERMISSION_TYPES`.
pub mod permissions {
    pub const READ: &str = "read";
    pub const WRITE: &str = "write";
    pub const DELETE: &str = "delete";
    pub const SHARE: &str = "share";

    pub const ALL: &[&str] = &[READ, WRITE, DELETE, SHARE];
}
