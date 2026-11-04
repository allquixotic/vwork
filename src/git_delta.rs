//! Git pack copy/insert instructions, without an entropy codec.
//! Bounded hash indexing and candidate probing keep encoding work linear.
use anyhow::{Context, Result, ensure};
use std::collections::HashMap;

pub const MAX_DEPTH: u8 = 4;
pub const MAX_DELTA_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_OBJECT_BYTES: usize = 1024 * 1024 * 1024;
const BLOCK: usize = 32;
const MAX_INDEX: usize = 65_536;

fn size(out: &mut Vec<u8>, mut n: usize) {
    loop {
        let mut b = (n & 127) as u8;
        n >>= 7;
        if n != 0 {
            b |= 128;
        }
        out.push(b);
        if n == 0 {
            break;
        }
    }
}
fn read_size(input: &mut &[u8]) -> Result<usize> {
    let mut n = 0usize;
    for shift in (0..usize::BITS).step_by(7) {
        let b = *input.first().context("truncated Git delta size")?;
        *input = &input[1..];
        let part = (b & 127) as usize;
        ensure!(part <= (usize::MAX >> shift), "Git delta size overflow");
        n |= part << shift;
        if b & 128 == 0 {
            return Ok(n);
        }
    }
    anyhow::bail!("Git delta size overflow")
}
fn literal(out: &mut Vec<u8>, bytes: &[u8]) {
    for chunk in bytes.chunks(127) {
        out.push(chunk.len() as u8);
        out.extend_from_slice(chunk);
    }
}
fn copy(out: &mut Vec<u8>, mut offset: usize, mut len: usize) {
    while len > 0 {
        let n = len.min(0xff_ffff);
        let at = out.len();
        out.push(128);
        for (i, b) in (offset as u32).to_le_bytes().iter().enumerate() {
            if *b != 0 {
                out[at] |= 1 << i;
                out.push(*b);
            }
        }
        for (i, b) in (n as u32).to_le_bytes()[..3].iter().enumerate() {
            if *b != 0 {
                out[at] |= 1 << (i + 4);
                out.push(*b);
            }
        }
        offset += n;
        len -= n;
    }
}
fn fingerprint(bytes: &[u8]) -> u64 {
    // Fixed-width input and bounded collision probing, not an identity hash.
    bytes.iter().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    })
}
pub fn encode(base: &[u8], target: &[u8]) -> Option<Vec<u8>> {
    if base.len() < 256
        || target.len() < 256
        || base.len() > MAX_DELTA_BYTES
        || target.len() > MAX_DELTA_BYTES
    {
        return None;
    }
    let stride = (base.len() / MAX_INDEX).max(BLOCK);
    let mut index: HashMap<u64, usize> =
        HashMap::with_capacity((base.len() / stride).min(MAX_INDEX));
    for offset in (0..=base.len() - BLOCK).step_by(stride) {
        index
            .entry(fingerprint(&base[offset..offset + BLOCK]))
            .or_insert(offset);
    }
    let mut out = Vec::new();
    size(&mut out, base.len());
    size(&mut out, target.len());
    let mut cursor = 0;
    let mut pending = 0;
    while cursor + BLOCK <= target.len() {
        if let Some(&offset) = index.get(&fingerprint(&target[cursor..cursor + BLOCK]))
            && base[offset..offset + BLOCK] == target[cursor..cursor + BLOCK]
        {
            let mut len = BLOCK;
            let max = (base.len() - offset).min(target.len() - cursor);
            while len < max && base[offset + len] == target[cursor + len] {
                len += 1;
            }
            literal(&mut out, &target[pending..cursor]);
            copy(&mut out, offset, len);
            cursor += len;
            pending = cursor;
            continue;
        }
        cursor += 1;
        if out.len() >= target.len() * 3 / 4 {
            return None;
        }
    }
    literal(&mut out, &target[pending..]);
    (out.len() + 32 < target.len() * 3 / 4).then_some(out)
}
pub fn apply(base: &[u8], mut delta: &[u8], expected: usize) -> Result<Vec<u8>> {
    ensure!(
        read_size(&mut delta)? == base.len(),
        "Git delta base length mismatch"
    );
    ensure!(
        read_size(&mut delta)? == expected && expected <= MAX_OBJECT_BYTES,
        "Git delta result length mismatch"
    );
    let mut out = Vec::with_capacity(expected);
    while let Some((&op, tail)) = delta.split_first() {
        delta = tail;
        if op & 128 != 0 {
            let mut offset = 0usize;
            let mut len = 0usize;
            for bit in 0..7 {
                if op & (1 << bit) != 0 {
                    let byte = *delta.first().context("truncated Git delta copy")? as usize;
                    delta = &delta[1..];
                    if bit < 4 {
                        offset |= byte << (bit * 8);
                    } else {
                        len |= byte << ((bit - 4) * 8);
                    }
                }
            }
            if len == 0 {
                len = 0x10000;
            }
            ensure!(
                offset <= base.len() && len <= base.len() - offset && len <= expected - out.len(),
                "Git delta copy out of bounds"
            );
            out.extend_from_slice(&base[offset..offset + len]);
        } else {
            let len = op as usize;
            ensure!(
                len > 0 && len <= delta.len() && len <= expected - out.len(),
                "invalid Git delta insert"
            );
            out.extend_from_slice(&delta[..len]);
            delta = &delta[len..];
        }
    }
    ensure!(out.len() == expected, "truncated Git delta result");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn v33_delta_roundtrip_bounds() {
        let base = (0..4000)
            .map(|i| format!("source line {i}\n"))
            .collect::<String>()
            .into_bytes();
        let mut target = base.clone();
        target.splice(
            200..211,
            b"changed content with a different length".iter().copied(),
        );
        let delta = encode(&base, &target).unwrap();
        assert!(delta.len() < target.len() / 10);
        assert_eq!(apply(&base, &delta, target.len()).unwrap(), target);
        assert!(apply(&base, &delta, target.len() + 1).is_err());
        // Standard Git opcode 0x80 copies 64 KiB from offset zero.
        let source = vec![7; 65536];
        let mut commands = Vec::new();
        size(&mut commands, 65536);
        size(&mut commands, 65536);
        commands.push(128);
        assert_eq!(apply(&source, &commands, 65536).unwrap(), source);
    }
    #[test]
    fn v33_corrupt_delta_fails_closed() {
        for bytes in [
            vec![],
            vec![0xff; 20],
            vec![1, 1, 0],
            vec![1, 1, 0x91, 2, 1],
            vec![1, 1, 2, 1, 2],
        ] {
            assert!(apply(&[1], &bytes, 1).is_err());
        }
    }
}
