//! Interface for decoding WOFF2 files

use bytes::Buf;
use thiserror::Error;

use crate::{
    checksum::{calculate_font_checksum_adjustment, set_checksum_adjustment, ChecksumError},
    magic_numbers::{TTF_CFF_FLAVOR, TTF_COLLECTION_FLAVOR, TTF_TRUE_TYPE_FLAVOR, WOFF2_SIGNATURE},
    ttf_header::{calculate_header_size, TableDirectory},
    woff2::{
        collection_directory::{CollectionHeader, CollectionHeaderError},
        header::{Woff2Header, Woff2HeaderError},
        table_directory::{TableDirectoryError, Woff2TableDirectory, WriteTablesError, HEAD_TAG},
    },
};

#[derive(Error, Debug)]
pub enum DecodeError {
    #[error("Invalid Woff2 File {0}")]
    Invalid(String),
    #[error("Unsupported feature {0}")]
    Unsupported(&'static str),
}

impl From<ChecksumError> for DecodeError {
    fn from(e: ChecksumError) -> Self {
        DecodeError::Invalid(e.to_string())
    }
}

impl From<CollectionHeaderError> for DecodeError {
    fn from(e: CollectionHeaderError) -> Self {
        DecodeError::Invalid(e.to_string())
    }
}

impl From<TableDirectoryError> for DecodeError {
    fn from(e: TableDirectoryError) -> Self {
        DecodeError::Invalid(e.to_string())
    }
}

impl From<Woff2HeaderError> for DecodeError {
    fn from(e: Woff2HeaderError) -> Self {
        DecodeError::Invalid(e.to_string())
    }
}

impl From<WriteTablesError> for DecodeError {
    fn from(e: WriteTablesError) -> Self {
        match e {
            WriteTablesError::Unsupported(e) => DecodeError::Unsupported(e),
            _ => DecodeError::Invalid(e.to_string()),
        }
    }
}

impl From<std::io::Error> for DecodeError {
    fn from(e: std::io::Error) -> Self {
        DecodeError::Invalid(e.to_string())
    }
}

/// Returns whether the buffer starts with the WOFF2 magic number.
pub fn is_woff2(input_buffer: &[u8]) -> bool {
    input_buffer.starts_with(&WOFF2_SIGNATURE.0)
}

/// Converts a WOFF2 font in `input_buffer` into a TTF format font.
pub fn convert_woff2_to_ttf(input_buffer: &mut impl Buf) -> Result<Vec<u8>, DecodeError> {
    let header = Woff2Header::from_buf(input_buffer)?;
    header.is_valid_header()?;

    if !matches!(
        header.flavor,
        TTF_COLLECTION_FLAVOR | TTF_CFF_FLAVOR | TTF_TRUE_TYPE_FLAVOR
    ) {
        Err(DecodeError::Invalid("Invalid font flavor".to_string()))?;
    }

    let table_directory = Woff2TableDirectory::from_buf(input_buffer, header.num_tables)?;

    let mut collection_header = if header.flavor == TTF_COLLECTION_FLAVOR {
        Some(CollectionHeader::from_buf(input_buffer, header.num_tables)?)
    } else {
        None
    };

    // for checking the compressed size
    let stream_start_remaining = input_buffer.remaining();

    let mut decompressed_tables =
        Vec::with_capacity(table_directory.uncompressed_length.try_into().unwrap());

    brotli::BrotliDecompress(&mut input_buffer.reader(), &mut decompressed_tables)?;

    let compressed_size = stream_start_remaining - input_buffer.remaining();

    if compressed_size != usize::try_from(header.total_compressed_size).unwrap() + 1 {
        Err(DecodeError::Invalid(
            "Compressed stream size does not match header".to_string(),
        ))?;
    }

    let mut out_buffer = Vec::with_capacity(header.total_sfnt_size as usize);
    // space for headers; we'll fill this in later once we've calculated table locations and
    // checksums
    let header_end = if let Some(collection_header) = &collection_header {
        collection_header.calculate_header_size()
    } else {
        calculate_header_size(table_directory.tables.len())
    };
    out_buffer.resize(header_end, 0);
    let ttf_tables = table_directory.write_to_buf(&mut out_buffer, &decompressed_tables)?;

    let mut header_buffer = &mut out_buffer[..header_end];
    if let Some(collection_header) = &mut collection_header {
        // sort tables for each font
        for font in &mut collection_header.fonts {
            font.table_indices
                .sort_unstable_by_key(|&idx| ttf_tables[idx as usize].tag.0);
        }
        collection_header.write_to_buf(&mut header_buffer, &ttf_tables);
    } else {
        let ttf_header = TableDirectory::new(header.flavor, ttf_tables);
        ttf_header.write_to_buf(&mut header_buffer);
        // calculate font checksum and store it at the appropriate location
        let head_table_record = ttf_header
            .find_table(HEAD_TAG)
            .ok_or_else(|| DecodeError::Invalid("Missing `head` table".into()))?;
        let checksum_adjustment = calculate_font_checksum_adjustment(&out_buffer);
        let head_table = &mut out_buffer[head_table_record.get_range()];
        set_checksum_adjustment(head_table, checksum_adjustment)?;
    }

    Ok(out_buffer)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use crate::test_resources::{FONTAWESOME_REGULAR_400, LATO_V22_LATIN_REGULAR};

    use super::convert_woff2_to_ttf;

    #[test]
    fn read_sample_font() {
        let buffer = LATO_V22_LATIN_REGULAR;
        let ttf = convert_woff2_to_ttf(&mut Cursor::new(buffer)).unwrap();
        assert_eq!(None, ttf_parser::fonts_in_collection(&ttf));
        let _parsed_ttf = ttf_parser::Face::from_slice(&ttf, 1).unwrap();
    }
    #[test]
    // Spec: https://www.w3.org/TR/WOFF2/#table_order
    // The loca table MUST follow the glyf table in the table directory. When WOFF2 file contains individually encoded font file, the table directory MAY contain other tables inserted between glyf and loca tables; however when WOFF2 contains a font collection file each loca table MUST immediately follow its corresponding glyf table. For example, the following order of tables: 'cmap', 'glyf', 'hhea', 'hmtx', 'loca', 'maxp' ... is acceptable for individually encoded font files;
    fn read_loca_is_not_after_glyf_font() {
        // In this test font, the loca table does not follow the glyf table.
        let buffer = FONTAWESOME_REGULAR_400;
        let ttf = convert_woff2_to_ttf(&mut Cursor::new(buffer)).unwrap();
        assert_eq!(None, ttf_parser::fonts_in_collection(&ttf));
        let _parsed_ttf = ttf_parser::Face::from_slice(&ttf, 1).unwrap();
    }

    #[test]
    fn sample_font_is_woff2() {
        assert!(super::is_woff2(LATO_V22_LATIN_REGULAR));
    }
}
