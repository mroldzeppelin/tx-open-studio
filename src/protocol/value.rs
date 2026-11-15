//! Value encoding (`B9/BA/BC/88/80/81/82`) and the tree walker.

pub(crate) const FLOAT_TAG: u8 = 0x88;

/// A preset index in the value encoding: literal below 128, `80 xx` above.
pub(crate) fn index_bytes(idx: u8) -> Vec<u8> {
    if idx >= 0x80 {
        vec![0x80, idx]
    } else {
        vec![idx]
    }
}

/// One node of a payload in the pedal's value encoding: `B9 n` / `BA n` =
/// list / array of n nodes, `BC n` = blob of n bytes, `88` = f32 LE,
/// anything else = an integer (literal < 0x80, `80 xx`, `81`/`82` u16 LE).
/// Lengths use the integer encoding too.
#[derive(Clone, Debug, PartialEq)]
pub enum Node<'a> {
    List(Vec<Node<'a>>),
    Blob(&'a [u8]),
    Float(f32),
    Int(u16),
}

/// Nesting limit for the tree walker (real dumps are ~5 deep).
pub(crate) const MAX_TREE_DEPTH: usize = 24;

pub(crate) fn parse_node<'a>(p: &'a [u8], pos: &mut usize, depth: usize) -> Option<Node<'a>> {
    if depth > MAX_TREE_DEPTH {
        return None;
    }
    match *p.get(*pos)? {
        0xB9 | 0xBA => {
            *pos += 1;
            let n = read_u16_value(p, pos)?;
            let mut items = Vec::with_capacity(usize::from(n).min(256));
            for _ in 0..n {
                items.push(parse_node(p, pos, depth + 1)?);
            }
            Some(Node::List(items))
        }
        0xBC => {
            *pos += 1;
            let n = usize::from(read_u16_value(p, pos)?);
            let blob = p.get(*pos..*pos + n)?;
            *pos += n;
            Some(Node::Blob(blob))
        }
        FLOAT_TAG => {
            let b = p.get(*pos + 1..*pos + 5)?;
            *pos += 5;
            Some(Node::Float(f32::from_le_bytes([b[0], b[1], b[2], b[3]])))
        }
        // Literal / 80 / 81 / 82 integers; any other tag fails the parse.
        _ => read_u16_value(p, pos).map(Node::Int),
    }
}

/// Parses the first node of `p` (None on any malformed or unknown item).
pub fn parse_tree(p: &[u8]) -> Option<Node<'_>> {
    parse_node(p, &mut 0, 0)
}

impl<'a> Node<'a> {
    /// The node at `path` (list indices from this node).
    pub fn at(&self, path: &[usize]) -> Option<&Node<'a>> {
        path.iter().try_fold(self, |node, &i| match node {
            Node::List(items) => items.get(i),
            _ => None,
        })
    }

    pub(crate) fn items(&self) -> Option<&[Node<'a>]> {
        match self {
            Node::List(items) => Some(items),
            _ => None,
        }
    }

    pub(crate) fn blob(&self) -> Option<&'a [u8]> {
        match self {
            Node::Blob(b) => Some(b),
            _ => None,
        }
    }

    pub(crate) fn int(&self) -> Option<u16> {
        match self {
            Node::Int(v) => Some(*v),
            _ => None,
        }
    }
}

pub(crate) fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

pub(crate) fn float_at(p: &[u8], pos: usize) -> Option<f32> {
    let b = p.get(pos..pos + 5)?;
    if b[0] != FLOAT_TAG {
        return None;
    }
    let v = f32::from_le_bytes([b[1], b[2], b[3], b[4]]);
    v.is_finite().then_some(v)
}

/// Reads one value at `*pos` in the `B9/BA/80/81/82/literal` encoding,
/// returning it as u16 (collections are not values; returns None).
pub(crate) fn read_u16_value(p: &[u8], pos: &mut usize) -> Option<u16> {
    let b = *p.get(*pos)?;
    match b {
        // One extra byte: u8 values 128..=255 (seen on the wire as
        // `80 8C` for preset index 140; the next byte after it is the
        // following value, not a high byte).
        0x80 => {
            let v = *p.get(*pos + 1)?;
            *pos += 2;
            Some(u16::from(v))
        }
        // Two extra bytes: u16 LE.
        0x81 | 0x82 => {
            let lo = *p.get(*pos + 1)?;
            let hi = *p.get(*pos + 2)?;
            *pos += 3;
            Some(u16::from(lo) | (u16::from(hi) << 8))
        }
        0x00..=0x7F => {
            *pos += 1;
            Some(u16::from(b))
        }
        _ => None,
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::test_util::*;

    #[test]
    fn value_tree_walker() {
        let mut p = enc_list(0xB9, vec![enc_int(5), enc_int(200), enc_int(1000), enc_blob(&[1, 2, 3]), enc_blob(&[9; 300])]);
        p.extend([0x88, 0, 0, 0x80, 0x3F]); // trailing node after the list: ignored
        let tree = parse_tree(&p).unwrap();
        assert_eq!(tree.at(&[0]), Some(&Node::Int(5)));
        assert_eq!(tree.at(&[1]), Some(&Node::Int(200)));
        assert_eq!(tree.at(&[2]), Some(&Node::Int(1000)));
        assert_eq!(tree.at(&[3]), Some(&Node::Blob(&[1, 2, 3])));
        assert_eq!(tree.at(&[4]).and_then(|n| n.blob()).map(|b| b.len()), Some(300));
        assert_eq!(tree.at(&[5]), None);
        assert_eq!(tree.at(&[0, 0]), None, "an int has no children");
        assert_eq!(parse_tree(&[0x88, 0, 0, 0x80, 0x3F]), Some(Node::Float(1.0)));
        // Nested lists and arrays.
        let nested = enc_list(0xB9, vec![enc_list(0xBA, vec![enc_int(1), enc_list(0xB9, vec![enc_int(42)])])]);
        assert_eq!(parse_tree(&nested).unwrap().at(&[0, 1, 0]), Some(&Node::Int(42)));
        // Malformed: truncated blob/list, unknown tag.
        assert!(parse_tree(&[0xBC, 0x05, 1, 2]).is_none());
        assert!(parse_tree(&[0xB9, 0x03, 0x01, 0x02]).is_none());
        assert!(parse_tree(&[0xB9, 0x01, 0x99]).is_none());
    }
}
