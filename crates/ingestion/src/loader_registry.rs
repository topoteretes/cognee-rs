/// Maps file extensions to loader engine names.
/// Even if we cannot process a file type yet, it must store the correct
/// loader_engine name for cross-SDK database compatibility.
pub fn get_loader_name(extension: &str) -> &'static str {
    match extension.to_lowercase().as_str() {
        // Text formats
        "txt" | "md" | "json" | "xml" | "yaml" | "yml" | "log" => "text_loader",
        // PDF
        "pdf" => "pypdf_loader",
        // Images
        "png" | "dwg" | "xcf" | "jpg" | "jpe" | "jpeg" | "jpx" | "apng" | "gif" | "webp"
        | "cr2" | "tif" | "tiff" | "bmp" | "jxr" | "psd" | "ico" | "heic" | "avif" => {
            "image_loader"
        }
        // Audio
        "aac" | "mid" | "mp3" | "m4a" | "ogg" | "flac" | "wav" | "amr" | "aiff" => "audio_loader",
        // CSV
        "csv" => "csv_loader",
        // Office / unstructured
        "docx" | "doc" | "odt" | "xlsx" | "xls" | "ods" | "pptx" | "ppt" | "odp" | "rtf"
        | "epub" | "eml" | "msg" => "unstructured_loader",
        // HTML
        "html" | "htm" => "beautiful_soup_loader",
        // Source code — treated as plain text
        "rs" | "py" | "js" | "ts" | "c" | "cpp" | "h" | "go" | "java" | "rb" | "sh" | "toml"
        | "cfg" | "ini" => "text_loader",
        // Default
        _ => "text_loader",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_extensions() {
        assert_eq!(get_loader_name("txt"), "text_loader");
        assert_eq!(get_loader_name("md"), "text_loader");
        assert_eq!(get_loader_name("TXT"), "text_loader");
    }

    #[test]
    fn test_pdf_extension() {
        assert_eq!(get_loader_name("pdf"), "pypdf_loader");
    }

    #[test]
    fn test_image_extensions() {
        assert_eq!(get_loader_name("png"), "image_loader");
        assert_eq!(get_loader_name("jpg"), "image_loader");
    }

    #[test]
    fn test_audio_extensions() {
        assert_eq!(get_loader_name("mp3"), "audio_loader");
        assert_eq!(get_loader_name("wav"), "audio_loader");
    }

    #[test]
    fn test_csv_extension() {
        assert_eq!(get_loader_name("csv"), "csv_loader");
    }

    #[test]
    fn test_office_extensions() {
        assert_eq!(get_loader_name("docx"), "unstructured_loader");
        assert_eq!(get_loader_name("pptx"), "unstructured_loader");
    }

    #[test]
    fn test_html_extension() {
        assert_eq!(get_loader_name("html"), "beautiful_soup_loader");
    }
}
