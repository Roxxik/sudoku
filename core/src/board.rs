use std::fmt;

pub const N: usize = 9;
pub const CELLS: usize = 81;
pub const ALL_DIGITS: u16 = 0x1FF;

pub type Digit = u8;
pub type CellIdx = usize;
pub type Mask = u16;

#[inline]
pub const fn digit_to_bit(d: Digit) -> Mask {
    1u16 << (d as u16 - 1)
}

#[inline]
pub const fn row_of(i: CellIdx) -> usize {
    i / 9
}

#[inline]
pub const fn col_of(i: CellIdx) -> usize {
    i % 9
}

#[inline]
pub const fn box_of(i: CellIdx) -> usize {
    (i / 9 / 3) * 3 + (i % 9) / 3
}

pub fn iter_digits(mask: Mask) -> impl Iterator<Item = Digit> {
    (1u8..=9).filter(move |&d| mask & digit_to_bit(d) != 0)
}

pub fn popcount(mask: Mask) -> u32 {
    (mask & ALL_DIGITS).count_ones()
}

pub fn cell_name(i: CellIdx) -> String {
    format!("R{}C{}", row_of(i) + 1, col_of(i) + 1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitKind {
    Row,
    Col,
    Box,
}

#[derive(Debug)]
pub enum ParseError {
    WrongLength(usize),
    InvalidChar(char),
    Conflict { cell: CellIdx, digit: Digit },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::WrongLength(n) => write!(f, "expected 81 cells, got {}", n),
            ParseError::InvalidChar(c) => write!(f, "invalid character: {:?}", c),
            ParseError::Conflict { cell, digit } => {
                write!(f, "digit {} at {} conflicts with a peer", digit, cell_name(*cell))
            }
        }
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, Clone)]
pub struct Board {
    cells: [Digit; CELLS],
    candidates: [Mask; CELLS],
}

impl Board {
    pub fn empty() -> Self {
        Self {
            cells: [0; CELLS],
            candidates: [ALL_DIGITS; CELLS],
        }
    }

    pub fn parse(s: &str) -> Result<Self, ParseError> {
        let chars: Vec<char> = s
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '|' && *c != '-' && *c != '+')
            .collect();
        if chars.len() != CELLS {
            return Err(ParseError::WrongLength(chars.len()));
        }
        let mut board = Self::empty();
        for (i, &c) in chars.iter().enumerate() {
            let d = match c {
                '.' | '0' => continue,
                '1'..='9' => c.to_digit(10).unwrap() as Digit,
                _ => return Err(ParseError::InvalidChar(c)),
            };
            let bit = digit_to_bit(d);
            if board.candidates[i] & bit == 0 {
                return Err(ParseError::Conflict { cell: i, digit: d });
            }
            board.place(i, d);
        }
        Ok(board)
    }

    #[inline]
    pub fn cell(&self, i: CellIdx) -> Digit {
        self.cells[i]
    }

    #[inline]
    pub fn candidates(&self, i: CellIdx) -> Mask {
        self.candidates[i]
    }

    #[inline]
    pub fn is_empty(&self, i: CellIdx) -> bool {
        self.cells[i] == 0
    }

    pub fn place(&mut self, i: CellIdx, d: Digit) {
        debug_assert!(self.cells[i] == 0, "placing on a filled cell");
        debug_assert!(
            self.candidates[i] & digit_to_bit(d) != 0,
            "placing impossible digit"
        );
        self.cells[i] = d;
        self.candidates[i] = 0;
        let bit = digit_to_bit(d);
        for &peer in &PEERS[i] {
            self.candidates[peer] &= !bit;
        }
    }

    pub fn clear(&mut self, i: CellIdx) {
        if self.cells[i] == 0 {
            return;
        }
        self.cells[i] = 0;
        self.recompute_candidates();
    }

    pub fn recompute_candidates(&mut self) {
        for i in 0..CELLS {
            self.candidates[i] = if self.cells[i] != 0 { 0 } else { ALL_DIGITS };
        }
        for i in 0..CELLS {
            let d = self.cells[i];
            if d == 0 {
                continue;
            }
            let bit = digit_to_bit(d);
            for &peer in &PEERS[i] {
                self.candidates[peer] &= !bit;
            }
        }
    }

    pub fn eliminate(&mut self, i: CellIdx, d: Digit) -> bool {
        let bit = digit_to_bit(d);
        if self.candidates[i] & bit == 0 {
            return false;
        }
        self.candidates[i] &= !bit;
        true
    }

    pub fn is_solved(&self) -> bool {
        self.cells.iter().all(|&c| c != 0)
    }

    pub fn to_line(&self) -> String {
        self.cells
            .iter()
            .map(|&c| if c == 0 { '.' } else { (b'0' + c) as char })
            .collect()
    }

    pub fn pretty(&self) -> String {
        let mut s = String::new();
        for r in 0..9 {
            if r > 0 && r % 3 == 0 {
                s.push_str("------+-------+------\n");
            }
            for c in 0..9 {
                if c > 0 && c % 3 == 0 {
                    s.push_str("| ");
                }
                let v = self.cells[r * 9 + c];
                s.push(if v == 0 { '.' } else { (b'0' + v) as char });
                s.push(' ');
            }
            s.push('\n');
        }
        s
    }
}

impl fmt::Display for Board {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.pretty())
    }
}

const fn build_row_units() -> [[CellIdx; 9]; 9] {
    let mut u = [[0usize; 9]; 9];
    let mut r = 0;
    while r < 9 {
        let mut c = 0;
        while c < 9 {
            u[r][c] = r * 9 + c;
            c += 1;
        }
        r += 1;
    }
    u
}

const fn build_col_units() -> [[CellIdx; 9]; 9] {
    let mut u = [[0usize; 9]; 9];
    let mut c = 0;
    while c < 9 {
        let mut r = 0;
        while r < 9 {
            u[c][r] = r * 9 + c;
            r += 1;
        }
        c += 1;
    }
    u
}

const fn build_box_units() -> [[CellIdx; 9]; 9] {
    let mut u = [[0usize; 9]; 9];
    let mut b = 0;
    while b < 9 {
        let br = (b / 3) * 3;
        let bc = (b % 3) * 3;
        let mut k = 0;
        let mut dr = 0;
        while dr < 3 {
            let mut dc = 0;
            while dc < 3 {
                u[b][k] = (br + dr) * 9 + (bc + dc);
                k += 1;
                dc += 1;
            }
            dr += 1;
        }
        b += 1;
    }
    u
}

const fn build_peers() -> [[CellIdx; 20]; 81] {
    let mut peers = [[0usize; 20]; 81];
    let mut i = 0;
    while i < 81 {
        let row = i / 9;
        let col = i % 9;
        let br = (row / 3) * 3;
        let bc = (col / 3) * 3;
        let mut k = 0;
        let mut j = 0;
        while j < 81 {
            if j != i {
                let jr = j / 9;
                let jc = j % 9;
                let same_row = jr == row;
                let same_col = jc == col;
                let same_box = jr >= br && jr < br + 3 && jc >= bc && jc < bc + 3;
                if same_row || same_col || same_box {
                    peers[i][k] = j;
                    k += 1;
                }
            }
            j += 1;
        }
        i += 1;
    }
    peers
}

pub const ROW_UNITS: [[CellIdx; 9]; 9] = build_row_units();
pub const COL_UNITS: [[CellIdx; 9]; 9] = build_col_units();
pub const BOX_UNITS: [[CellIdx; 9]; 9] = build_box_units();
pub const PEERS: [[CellIdx; 20]; 81] = build_peers();

pub fn all_units() -> impl Iterator<Item = (UnitKind, usize, &'static [CellIdx; 9])> {
    ROW_UNITS
        .iter()
        .enumerate()
        .map(|(i, u)| (UnitKind::Row, i, u))
        .chain(
            COL_UNITS
                .iter()
                .enumerate()
                .map(|(i, u)| (UnitKind::Col, i, u)),
        )
        .chain(
            BOX_UNITS
                .iter()
                .enumerate()
                .map(|(i, u)| (UnitKind::Box, i, u)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn peers_are_20_and_distinct() {
        for i in 0..81 {
            let mut seen = HashSet::new();
            for &p in &PEERS[i] {
                assert_ne!(p, i, "cell is its own peer at {}", i);
                assert!(seen.insert(p), "duplicate peer for cell {}", i);
            }
            assert_eq!(seen.len(), 20, "wrong peer count for cell {}", i);
        }
    }

    #[test]
    fn round_trip_parse() {
        let s = "003020600900305001001806400008102900700000008006708200002609500800203009005010300";
        let b = Board::parse(s).unwrap();
        assert_eq!(b.to_line(), s.replace('0', "."));
    }

    #[test]
    fn parse_rejects_peer_conflict() {
        let s = "550000000000000000000000000000000000000000000000000000000000000000000000000000000";
        assert!(Board::parse(s).is_err());
    }

    #[test]
    fn placing_strips_peer_candidates() {
        let mut b = Board::empty();
        b.place(0, 5);
        for &p in &PEERS[0] {
            assert_eq!(b.candidates(p) & digit_to_bit(5), 0);
        }
    }

    #[test]
    fn box_of_is_correct() {
        assert_eq!(box_of(0), 0);
        assert_eq!(box_of(8), 2);
        assert_eq!(box_of(40), 4);
        assert_eq!(box_of(80), 8);
    }
}
