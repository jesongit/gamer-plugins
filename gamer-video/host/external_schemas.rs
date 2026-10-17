//! Test and packaging fixtures consume the exact same declared schema bytes.
#[cfg(test)]
pub(crate) fn append_schemas<W: std::io::Write + std::io::Seek>(writer: &mut zip::ZipWriter<W>) {
    use std::io::Write;
    let options = zip::write::SimpleFileOptions::default();
    writer
        .start_file("schemas/external_sample_list.json", options)
        .unwrap();
    writer
        .write_all(include_bytes!("../schemas/external_sample_list.json"))
        .unwrap();
    writer
        .start_file("schemas/external_sample_read.json", options)
        .unwrap();
    writer
        .write_all(include_bytes!("../schemas/external_sample_read.json"))
        .unwrap();
    writer
        .start_file("schemas/external_sample_frames.json", options)
        .unwrap();
    writer
        .write_all(include_bytes!("../schemas/external_sample_frames.json"))
        .unwrap();
    writer
        .start_file("schemas/external_sample_events.json", options)
        .unwrap();
    writer
        .write_all(include_bytes!("../schemas/external_sample_events.json"))
        .unwrap();
    writer
        .start_file("schemas/external_sample_frame.json", options)
        .unwrap();
    writer
        .write_all(include_bytes!("../schemas/external_sample_frame.json"))
        .unwrap();
    writer
        .start_file("schemas/external_sample_export.json", options)
        .unwrap();
    writer
        .write_all(include_bytes!("../schemas/external_sample_export.json"))
        .unwrap();
    writer
        .start_file("schemas/external_sample_import.json", options)
        .unwrap();
    writer
        .write_all(include_bytes!("../schemas/external_sample_import.json"))
        .unwrap();
    writer
        .start_file("schemas/external_sample_create.json", options)
        .unwrap();
    writer
        .write_all(include_bytes!("../schemas/external_sample_create.json"))
        .unwrap();
}
