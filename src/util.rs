
const KB: u64 = 1024;
const MB: u64 = 1024 * 1024;

pub fn format_size(bytes: u64) -> String {
    if bytes < MB {
        format!("{} kb", bytes / KB)
    }
    else {
        format!("{} mb", bytes / MB)
    }
}