//! Versioned VST 3 state envelope. VST 2 chunks retain their native representation.
use crate::PluginError;
const MAGIC: &[u8; 5] = b"SSV3\x01";
const LIMIT: usize = 64 * 1024 * 1024 - 8;

pub fn encode(component: &[u8], controller: &[u8]) -> Result<Vec<u8>, PluginError> {
    if component
        .len()
        .saturating_add(controller.len())
        .saturating_add(13)
        > LIMIT
    {
        return Err(PluginError("VST 3 state too large".into()));
    }
    let mut blob = Vec::with_capacity(13 + component.len() + controller.len());
    blob.extend_from_slice(MAGIC);
    for bytes in [component, controller] {
        blob.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        blob.extend_from_slice(bytes);
    }
    Ok(blob)
}

pub fn decode(blob: &[u8]) -> Result<(&[u8], &[u8]), PluginError> {
    let invalid = || PluginError("invalid VST 3 state magic, version or framing".into());
    if blob.len() > LIMIT || !blob.starts_with(MAGIC) {
        return Err(invalid());
    }
    let mut rest = &blob[MAGIC.len()..];
    fn part<'a>(rest: &mut &'a [u8]) -> Option<&'a [u8]> {
        let length = u32::from_le_bytes(rest.get(..4)?.try_into().ok()?) as usize;
        *rest = &rest[4..];
        let bytes = rest.get(..length)?;
        *rest = &rest[length..];
        Some(bytes)
    }
    let component = part(&mut rest).ok_or_else(invalid)?;
    let controller = part(&mut rest).ok_or_else(invalid)?;
    if !rest.is_empty() {
        return Err(invalid());
    }
    Ok((component, controller))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vst3_state_framing_round_trip() {
        let blob = encode(&[1, 3], &[4, 5, 6]).unwrap();
        assert_eq!(decode(&blob).unwrap(), (&[1, 3][..], &[4, 5, 6][..]));
        for length in 0..blob.len() {
            assert!(decode(&blob[..length]).is_err());
        }
        assert!(decode(&[1, 3]).is_err());
        let mut corrupt = blob.clone();
        corrupt[4] = 2;
        assert!(decode(&corrupt).is_err());
        corrupt = blob.clone();
        corrupt.extend_from_slice(&[0]);
        assert!(decode(&corrupt).is_err());
        assert_eq!(
            decode(&encode(&[], &[]).unwrap()).unwrap(),
            (&[][..], &[][..])
        );
    }
}
