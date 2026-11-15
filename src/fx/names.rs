//! VIR cab model names and the cab model selector options.

/// VIR cab names in index order, as in the official TONEX software's cab
/// list; order verified on the pedal at indices 0, 25 and 39.
pub const VIR_CAB_NAMES: [&str; 40] = [
    "16 DK", "18 CHAMP", "110 TONE", "112 DLX", "112 MARK", "112 CALI", "112 MAZ", "112 TINY", "115 BMAN",
    "212 TWIN", "212 BLUE", "212 H70", "212 JAZZ", "212 OPEN", "212 PPC", "212 REHO", "212 DZ", "412 1960",
    "412 B30", "412 B8000", "412 B9000", "412 BS", "412 DBAG", "412 HW", "412 F1", "412 T1", "412 V1",
    "412 M1", "412 PPC", "412 RETS", "412 RKB", "412 RKV", "412 PIG", "412 GRN", "412 XXL", "412 STD",
    "410 SVX", "115 SVX", "810 SVX", "810 OBC",
];

/// Cab model selector options: the named models, extended with numbered
/// entries ("Cab 41", 1-based) up to `current` if that lies beyond the
/// table, so any index the pedal reports stays selectable.
pub fn cab_model_options(current: usize) -> Vec<String> {
    let len = VIR_CAB_NAMES.len().max(current + 1);
    (0..len)
        .map(|i| VIR_CAB_NAMES.get(i).map_or_else(|| format!("Cab {}", i + 1), |n| n.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::CAB_MODELS;

    #[test]
    fn vir_cab_names_and_fallback() {
        // Verified on the pedal: 0, 25 and 39.
        assert_eq!(VIR_CAB_NAMES[0], "16 DK");
        assert_eq!(VIR_CAB_NAMES[25], "412 T1");
        assert_eq!(VIR_CAB_NAMES[39], "810 OBC");
        let named = cab_model_options(0);
        assert_eq!(named.len(), 40);
        assert_eq!(named[25], "412 T1");
        assert_eq!(cab_model_options(39).len(), 40);
        // Beyond the table: numbered entries up to the current index.
        let extended = cab_model_options(45);
        assert_eq!(extended.len(), 46);
        assert_eq!(extended[39], "810 OBC");
        assert_eq!(extended[40], "Cab 41");
        assert_eq!(extended[45], "Cab 46");
        assert!(VIR_CAB_NAMES.len() < CAB_MODELS, "reads past the table stay accepted");
    }
}
