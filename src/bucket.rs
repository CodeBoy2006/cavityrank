pub(crate) const SLOTS: usize = 4;
const SLOT_MASK: u64 = u16::MAX as u64;

/// One packed `u64` containing up to four nonzero 16-bit fingerprints.
/// Occupied slots form a low-order prefix; zero marks the unused suffix.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub(crate) struct Bucket(u64);

impl Bucket {
    #[inline]
    pub(crate) fn slot(self, index: usize) -> u16 {
        debug_assert!(index < SLOTS);
        ((self.0 >> (16 * index)) & SLOT_MASK) as u16
    }

    #[inline]
    pub(crate) fn occupancy(self) -> usize {
        if self.slot(3) != 0 {
            4
        } else if self.slot(2) != 0 {
            3
        } else if self.slot(1) != 0 {
            2
        } else {
            usize::from(self.slot(0) != 0)
        }
    }

    /// Ranked insertion benefits from fixed-cost CLZ, while ordinary kicks
    /// keep `occupancy`'s cheaper early exit for the common full-bucket case.
    #[inline]
    pub(crate) fn branchless_occupancy(self) -> usize {
        SLOTS - self.0.leading_zeros() as usize / 16
    }

    #[inline]
    pub(crate) fn is_full(self) -> bool {
        self.0 >> 48 != 0
    }

    #[inline]
    pub(crate) fn contains(self, fingerprint: u16) -> bool {
        self.slot(0) == fingerprint
            || self.slot(1) == fingerprint
            || self.slot(2) == fingerprint
            || self.slot(3) == fingerprint
    }

    #[inline]
    pub(crate) fn append(&mut self, fingerprint: u16) -> bool {
        debug_assert_ne!(fingerprint, 0);
        let occupancy = self.occupancy();
        if occupancy == SLOTS {
            return false;
        }
        self.0 |= u64::from(fingerprint) << (16 * occupancy);
        true
    }

    #[inline]
    pub(crate) fn replace(&mut self, slot: usize, fingerprint: u16) {
        debug_assert!(slot < SLOTS);
        let shift = 16 * slot;
        let mask = SLOT_MASK << shift;
        self.0 = (self.0 & !mask) | (u64::from(fingerprint) << shift);
    }

    pub(crate) fn remove_first(&mut self, fingerprint: u16) -> bool {
        debug_assert_ne!(fingerprint, 0);
        let Some(position) = self
            .slots()
            .iter()
            .position(|&resident| resident == fingerprint)
        else {
            return false;
        };

        let low_mask = (1_u64 << (16 * position)).wrapping_sub(1);
        // Shift later fingerprints down to preserve the occupied-prefix invariant.
        self.0 = (self.0 & low_mask) | ((self.0 >> 16) & !low_mask);
        true
    }

    #[inline]
    pub(crate) fn slots(self) -> [u16; SLOTS] {
        [self.slot(0), self.slot(1), self.slot(2), self.slot(3)]
    }

    #[inline]
    pub(crate) fn from_slots(slots: [u16; SLOTS]) -> Self {
        Self(
            u64::from(slots[0])
                | (u64::from(slots[1]) << 16)
                | (u64::from(slots[2]) << 32)
                | (u64::from(slots[3]) << 48),
        )
    }

    pub(crate) fn sorted_slots(self) -> [u16; SLOTS] {
        let mut slots = self.slots();
        slots.sort_unstable();
        slots
    }

    /// Decodes 1 = NEAR, 2 = FAR with one 16-bit comparison.
    #[inline]
    pub(crate) fn decode_cavity(self) -> u8 {
        1 + u8::from(self.slot(1) < self.slot(0))
    }

    pub(crate) fn encode_cavity(mut slots: [u16; SLOTS], state: u8) -> Self {
        debug_assert!(slots.iter().all(|&fingerprint| fingerprint != 0));
        let descending = state.clamp(1, 2) == 2;
        if slots[0] == slots[1] {
            let distinct = if slots[2] != slots[0] {
                2
            } else if slots[3] != slots[0] {
                3
            } else {
                return Self::from_slots(slots);
            };
            slots.swap(1, distinct);
        }
        if (slots[1] < slots[0]) != descending {
            slots.swap(0, 1);
        }
        Self::from_slots(slots)
    }

    /// Decodes a truncated rank in 1..=4 from two pair orientations.
    #[inline]
    pub(crate) fn decode_rank4(self) -> u8 {
        1 + u8::from(self.slot(1) < self.slot(0)) + 2 * u8::from(self.slot(3) < self.slot(2))
    }

    pub(crate) fn encode_rank4(mut slots: [u16; SLOTS], rank: u8) -> Self {
        debug_assert!(slots.iter().all(|&fingerprint| fingerprint != 0));
        if slots[0] == slots[1] || slots[2] == slots[3] {
            slots.sort_unstable();
            if slots[0] != slots[2] && slots[1] != slots[3] {
                slots = [slots[0], slots[2], slots[1], slots[3]];
            }
        }
        if slots[0] == slots[1] || slots[2] == slots[3] {
            // Three or four equal fingerprints cannot form two independently
            // orientable pairs. The rank may alias, but the multiset is preserved.
            return Self::from_slots(slots);
        }

        let state = rank.clamp(1, 4) - 1;
        if (slots[1] < slots[0]) != (state & 1 != 0) {
            slots.swap(0, 1);
        }
        if (slots[3] < slots[2]) != (state & 2 != 0) {
            slots.swap(2, 3);
        }
        Self::from_slots(slots)
    }

    pub(crate) fn decode_d4(self) -> u8 {
        let slots = self.slots();
        for left in 0..SLOTS {
            for right in left + 1..SLOTS {
                if slots[left] == slots[right] {
                    return 4;
                }
            }
        }

        let minimum = slots
            .iter()
            .enumerate()
            .min_by_key(|&(_, fingerprint)| fingerprint)
            .map_or(0, |(index, _)| index);
        let clockwise = (minimum + 1) & 3;
        let counterclockwise = (minimum + 3) & 3;
        let orientation = u8::from(slots[clockwise] > slots[counterclockwise]);
        1 + minimum as u8 + 4 * orientation
    }

    pub(crate) fn encode_d4(mut slots: [u16; SLOTS], potential: u8) -> Self {
        for left in 0..SLOTS {
            for right in left + 1..SLOTS {
                if slots[left] == slots[right] {
                    slots.sort_unstable();
                    return Self::from_slots(slots);
                }
            }
        }

        let minimum = slots
            .iter()
            .enumerate()
            .min_by_key(|&(_, fingerprint)| fingerprint)
            .map_or(0, |(index, _)| index);
        let state = potential.clamp(1, 8) - 1;
        let desired_position = usize::from(state & 3);
        let desired_orientation = state >> 2;
        let clockwise = (minimum + 1) & 3;
        let counterclockwise = (minimum + 3) & 3;
        let orientation = u8::from(slots[clockwise] > slots[counterclockwise]);
        if orientation != desired_orientation {
            slots.swap(clockwise, counterclockwise);
        }

        let shift = (desired_position + SLOTS - minimum) % SLOTS;
        let mut encoded = [0; SLOTS];
        for (index, fingerprint) in slots.into_iter().enumerate() {
            encoded[(index + shift) & 3] = fingerprint;
        }
        Self::from_slots(encoded)
    }
}

#[cfg(test)]
mod tests {
    use super::{Bucket, SLOTS};

    #[test]
    fn bucket_is_exactly_one_word() {
        assert_eq!(size_of::<Bucket>(), size_of::<u64>());
    }

    #[test]
    fn compact_operations_preserve_order() {
        let mut bucket = Bucket::default();
        for fingerprint in [7, 11, 13, 17] {
            assert!(bucket.append(fingerprint));
        }
        assert!(bucket.is_full());
        assert!(bucket.contains(13));
        assert!(bucket.remove_first(11));
        assert_eq!(bucket.slots(), [7, 13, 17, 0]);
        assert_eq!(bucket.occupancy(), 3);
        assert!(!bucket.remove_first(19));

        for (fingerprint, expected) in [
            (7, [11, 13, 17, 0]),
            (11, [7, 13, 17, 0]),
            (13, [7, 11, 17, 0]),
            (17, [7, 11, 13, 0]),
        ] {
            let mut bucket = Bucket::from_slots([7, 11, 13, 17]);
            assert!(bucket.remove_first(fingerprint));
            assert_eq!(bucket.slots(), expected);
        }

        let mut duplicates = Bucket::from_slots([7, 11, 11, 17]);
        assert!(duplicates.remove_first(11));
        assert_eq!(duplicates.slots(), [7, 11, 17, 0]);
    }

    #[test]
    fn pair_cavity_codec_round_trips_all_unique_permutations() {
        for a in 1..=SLOTS as u16 {
            for b in 1..=SLOTS as u16 {
                for c in 1..=SLOTS as u16 {
                    for d in 1..=SLOTS as u16 {
                        let input = [a, b, c, d];
                        if a == b || a == c || a == d || b == c || b == d || c == d {
                            continue;
                        }
                        let mut unique = input;
                        unique.sort_unstable();
                        for state in [1, 2] {
                            let encoded = Bucket::encode_cavity(input, state);
                            assert_eq!(encoded.decode_cavity(), state);
                            let mut payload = encoded.slots();
                            payload.sort_unstable();
                            assert_eq!(payload, unique);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn rank4_codec_round_trips_every_encodable_multiset() {
        for a in 1..=SLOTS as u16 {
            for b in 1..=SLOTS as u16 {
                for c in 1..=SLOTS as u16 {
                    for d in 1..=SLOTS as u16 {
                        let input = [a, b, c, d];
                        if input
                            .iter()
                            .any(|value| input.iter().filter(|other| *other == value).count() >= 3)
                        {
                            continue;
                        }
                        let mut expected = input;
                        expected.sort_unstable();
                        for rank in 1..=4 {
                            let encoded = Bucket::encode_rank4(input, rank);
                            assert_eq!(encoded.decode_rank4(), rank);
                            let mut actual = encoded.slots();
                            actual.sort_unstable();
                            assert_eq!(actual, expected);
                        }
                    }
                }
            }
        }

        let mut payload = Bucket::encode_rank4([7, 7, 7, 11], 4).slots();
        payload.sort_unstable();
        assert_eq!(payload, [7, 7, 7, 11]);
    }

    #[test]
    fn rank4_encoding_never_changes_query_results() {
        for a in 1..=SLOTS as u16 {
            for b in 1..=SLOTS as u16 {
                for c in 1..=SLOTS as u16 {
                    for d in 1..=SLOTS as u16 {
                        let original = Bucket::from_slots([a, b, c, d]);
                        for rank in 1..=4 {
                            let encoded = Bucket::encode_rank4(original.slots(), rank);
                            for fingerprint in 1..=SLOTS as u16 + 1 {
                                assert_eq!(
                                    encoded.contains(fingerprint),
                                    original.contains(fingerprint)
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn rank4_aliases_only_when_three_fingerprints_are_equal() {
        let mut aliased_inputs = 0;
        for a in 1..=SLOTS as u16 {
            for b in 1..=SLOTS as u16 {
                for c in 1..=SLOTS as u16 {
                    for d in 1..=SLOTS as u16 {
                        let input = [a, b, c, d];
                        let aliases = input
                            .iter()
                            .any(|value| input.iter().filter(|other| *other == value).count() >= 3);
                        aliased_inputs += usize::from(aliases);
                        for rank in 1..=4 {
                            let encoded = Bucket::encode_rank4(input, rank);
                            if aliases {
                                assert_eq!(encoded.decode_rank4(), 1);
                            } else {
                                assert_eq!(encoded.decode_rank4(), rank);
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(aliased_inputs, 52);
    }

    #[test]
    fn d4_codec_round_trips_all_states() {
        let payload = [19, 3, 41, 7];
        let mut expected = payload;
        expected.sort_unstable();
        for state in 1..=8 {
            let encoded = Bucket::encode_d4(payload, state);
            assert_eq!(encoded.decode_d4(), state);
            let mut actual = encoded.slots();
            actual.sort_unstable();
            assert_eq!(actual, expected);
        }
    }
}
