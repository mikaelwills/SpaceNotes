use anyhow::{bail, Result};

const TAG_PKESK: u8 = 1;
const KEY_ID_OFFSET: usize = 1;
const KEY_ID_LEN: usize = 8;

pub fn recipient_key_ids(bytes: &[u8]) -> Result<Vec<String>> {
    if bytes.is_empty() {
        bail!("Not an OpenPGP message: no bytes");
    }

    let mut ids = Vec::new();
    let mut cursor = 0usize;
    let mut saw_packet = false;

    while cursor < bytes.len() {
        let header = bytes[cursor];
        if header & 0x80 == 0 {
            bail!("Not an OpenPGP message: byte {cursor} is not a packet header");
        }
        cursor += 1;

        let new_format = header & 0x40 != 0;
        let tag = if new_format {
            header & 0x3f
        } else {
            (header >> 2) & 0x0f
        };

        let body_len = if new_format {
            read_new_format_length(bytes, &mut cursor)?
        } else {
            read_old_format_length(bytes, &mut cursor, header & 0x03)?
        };

        let Some(body_len) = body_len else {
            saw_packet = true;
            break;
        };

        let body_end = cursor
            .checked_add(body_len)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| anyhow::anyhow!("Not an OpenPGP message: packet body overruns input"))?;

        if tag == TAG_PKESK {
            let id_end = KEY_ID_OFFSET + KEY_ID_LEN;
            if body_len < id_end {
                bail!("Not an OpenPGP message: PKESK packet is too short for a key id");
            }
            let id = &bytes[cursor + KEY_ID_OFFSET..cursor + id_end];
            ids.push(
                id.iter()
                    .map(|b| format!("{b:02X}"))
                    .collect::<Vec<_>>()
                    .join(""),
            );
        }

        saw_packet = true;
        cursor = body_end;
    }

    if !saw_packet {
        bail!("Not an OpenPGP message: no packets found");
    }
    if ids.is_empty() {
        bail!("Not an OpenPGP message: no public-key encrypted session key packets");
    }

    Ok(ids)
}

fn read_old_format_length(
    bytes: &[u8],
    cursor: &mut usize,
    length_type: u8,
) -> Result<Option<usize>> {
    let width = match length_type {
        0 => 1usize,
        1 => 2,
        2 => 4,
        _ => return Ok(None),
    };
    if *cursor + width > bytes.len() {
        bail!("Not an OpenPGP message: truncated packet length");
    }
    let mut len = 0usize;
    for _ in 0..width {
        len = (len << 8) | bytes[*cursor] as usize;
        *cursor += 1;
    }
    Ok(Some(len))
}

fn read_new_format_length(bytes: &[u8], cursor: &mut usize) -> Result<Option<usize>> {
    if *cursor >= bytes.len() {
        bail!("Not an OpenPGP message: truncated packet length");
    }
    let first = bytes[*cursor];
    *cursor += 1;

    if first < 192 {
        return Ok(Some(first as usize));
    }
    if first < 224 {
        if *cursor >= bytes.len() {
            bail!("Not an OpenPGP message: truncated two-octet length");
        }
        let second = bytes[*cursor];
        *cursor += 1;
        return Ok(Some(((first as usize - 192) << 8) + second as usize + 192));
    }
    if first < 255 {
        return Ok(None);
    }
    if *cursor + 4 > bytes.len() {
        bail!("Not an OpenPGP message: truncated five-octet length");
    }
    let mut len = 0usize;
    for _ in 0..4 {
        len = (len << 8) | bytes[*cursor] as usize;
        *cursor += 1;
    }
    Ok(Some(len))
}

pub fn parse_gpg_id(contents: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for line in contents.lines() {
        let id = line.trim().trim_end_matches('!').to_uppercase();
        if id.is_empty() || id.starts_with('#') || ids.contains(&id) {
            continue;
        }
        ids.push(id);
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    fn real_store_entry() -> Option<Vec<u8>> {
        let home = std::env::var("HOME").ok()?;
        let entry = format!("{home}/.password-store/secure.tesco.com.gpg");
        std::fs::read(entry).ok()
    }

    #[test]
    fn a_truncated_pkesk_hides_a_recipient_and_is_therefore_refused_upstream() {
        let ids: [[u8; 8]; 3] = [
            [0xC5, 0x82, 0xF8, 0xC6, 0x6A, 0x65, 0x9D, 0x51],
            [0x63, 0x3F, 0xB3, 0x1F, 0xF4, 0x29, 0x71, 0xF1],
            [0x4C, 0xC2, 0xB0, 0x68, 0x2D, 0x69, 0x55, 0x65],
        ];
        let mut bytes = Vec::new();
        for id in ids {
            bytes.extend_from_slice(&[0xc1, 0x0c, 0x03]);
            bytes.extend_from_slice(&id);
            bytes.extend_from_slice(&[0x12, 0x00, 0x00]);
        }
        bytes.extend_from_slice(&[0xc1, 0xe1, 0x03]);

        let found = recipient_key_ids(&bytes).expect("the walker stops, it does not panic");
        assert_eq!(
            found.len(),
            3,
            "a recipient hidden behind a partial-length header must not be counted"
        );
    }

    #[test]
    fn a_real_four_recipient_entry_yields_its_four_key_ids() {
        let Some(bytes) = real_store_entry() else {
            eprintln!(
                "SKIPPED a_real_four_recipient_entry_yields_its_four_key_ids: \
                 ~/.password-store/secure.tesco.com.gpg is absent, so this test proved \
                 nothing on this machine"
            );
            return;
        };
        let ids = recipient_key_ids(&bytes).expect("real entry should parse");
        assert_eq!(ids.len(), 4, "expected four PKESK packets, got {ids:?}");
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(
            sorted,
            vec![
                "29F11110A0624877".to_string(),
                "4CC2B0682D695565".to_string(),
                "633FB31FF42971F1".to_string(),
                "C582F8C66A659D51".to_string(),
            ]
        );
    }

    #[test]
    fn a_single_recipient_message_yields_exactly_one_key_id() {
        let mut bytes = vec![0xc1, 0x0c, 0x03];
        bytes.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF, 0xDE, 0xAD, 0xBE, 0xEF]);
        bytes.extend_from_slice(&[0x12, 0x00, 0x00]);
        bytes.extend_from_slice(&[0xd2, 0x01, 0x00]);

        let ids = recipient_key_ids(&bytes).expect("hand-built message should parse");
        assert_eq!(ids, vec!["DEADBEEFDEADBEEF".to_string()]);
    }

    #[test]
    fn garbage_bytes_are_refused_rather_than_panicking() {
        let err = recipient_key_ids(&[0x00, 0x01, 0x02, 0x03]).unwrap_err();
        assert!(
            err.to_string().contains("not a packet header"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn truncated_input_is_refused_rather_than_panicking() {
        let err = recipient_key_ids(&[0xc1, 0x0c, 0x03, 0xDE, 0xAD]).unwrap_err();
        assert!(
            err.to_string().contains("overruns"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn empty_input_is_refused() {
        assert!(recipient_key_ids(&[]).is_err());
    }

    #[test]
    fn a_message_without_pkesk_packets_is_refused() {
        let bytes = vec![0xd2, 0x02, 0x01, 0x00];
        let err = recipient_key_ids(&bytes).unwrap_err();
        assert!(
            err.to_string().contains("no public-key encrypted"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn gpg_id_parsing_strips_the_trailing_bang_and_uppercases() {
        let ids = parse_gpg_id("C582F8C66A659D51!\n633fb31ff42971f1\n\n# comment\n");
        assert_eq!(
            ids,
            vec![
                "C582F8C66A659D51".to_string(),
                "633FB31FF42971F1".to_string()
            ]
        );
    }
}

