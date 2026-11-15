//! Picker contents from the catalog: the AMP model list (brand · name)
//! and the CAB cascading menu (own cab / VIR ▸ size / Hardware Lib ▸
//! brand / IR / Unsorted / Off).

use super::*;
use crate::fx::VIR_CAB_NAMES;
use crate::protocol::{SlotChoice, KIND_AMP_MODEL};

/// Brands of more than one word, as they start a "based on" string.
const MULTI_WORD_BRANDS: [&str; 6] = ["Mesa Boogie", "Hughes & Kettner", "Dr. Z", "Bad Cat", "Two Rock", "Divided By 13"];

/// The brand a "based on" string starts with ("Fender Twin Reverb '65" →
/// "Fender"); "Other" without one.
pub(crate) fn brand_of(based_on: Option<&str>) -> String {
    let Some(text) = based_on.map(str::trim).filter(|t| !t.is_empty()) else {
        return "Other".into();
    };
    if let Some(b) = MULTI_WORD_BRANDS.iter().find(|b| text.to_lowercase().starts_with(&b.to_lowercase())) {
        return (*b).to_string();
    }
    text.split_whitespace().next().unwrap_or("Other").to_string()
}

/// Cabinet size of a VIR cab name ("412 GRN" → "4x12", "16 DK" → "1x6").
pub(crate) fn vir_size(name: &str) -> Option<String> {
    let digits = name.split_whitespace().next()?;
    let (count, size) = digits.split_at_checked(1)?;
    if size.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!("{count}x{size}"))
}

fn size_key(size: &str) -> (u32, u32) {
    let mut parts = size.split('x').map(|p| p.parse().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
}

/// One AMP model choice.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AmpOption {
    pub(crate) id: LibraryId,
    /// The stepper's value: the model name or "Amp 7".
    pub(crate) label: String,
    /// Brand of its based-on; None for placeholders (menu: "Unsorted").
    pub(crate) brand: Option<String>,
}

/// The AMP stepper's choices: listed tone models (and, with
/// `placeholders`, items not named yet), sorted by brand then name;
/// placeholders last, by library position. `current` (the AMP slot's item)
/// is appended with its name if it isn't listed.
pub(crate) fn amp_options(catalog: &Catalog, current: Option<(&LibraryId, &str)>, placeholders: bool) -> Vec<AmpOption> {
    let mut known = Vec::new();
    let mut unknown = Vec::new();
    for item in &catalog.list {
        match catalog.item(&item.id) {
            Some(c) if is_model_kind(c.kind) => {
                let brand = brand_of(c.based_on.as_deref());
                known.push((brand.to_lowercase(), c.name.to_lowercase(), AmpOption { id: item.id, label: c.name.clone(), brand: Some(brand) }));
            }
            Some(_) => {} // an IR
            None if !placeholders => {}
            None => {
                unknown.push(AmpOption { id: item.id, label: format!("Amp {}", item.position + 1), brand: None });
            }
        }
    }
    known.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    let mut out: Vec<AmpOption> = known.into_iter().map(|(.., o)| o).chain(unknown).collect();
    if let Some((id, name)) = current {
        if !out.iter().any(|o| o.id == *id) {
            out.push(AmpOption { id: *id, label: name.to_string(), brand: None });
        }
    }
    out
}

/// The AMP model menu: brand ▸ model (stomp captures under their brand
/// too), then "Unsorted" ▸ items without a name yet. Entry actions are
/// indices into `options` (already sorted by brand, then name).
pub(crate) fn amp_menu(options: &[AmpOption]) -> Vec<MenuNode> {
    let mut groups: Vec<MenuNode> = Vec::new();
    let mut unsorted = Vec::new();
    for (i, o) in options.iter().enumerate() {
        let leaf = MenuNode { label: o.label.clone(), action: Some(i), children: Vec::new() };
        match &o.brand {
            Some(brand) => match groups.iter_mut().find(|g| g.label == *brand) {
                Some(g) => g.children.push(leaf),
                None => groups.push(group(brand, vec![leaf])),
            },
            None => unsorted.push(leaf),
        }
    }
    if !unsorted.is_empty() {
        groups.push(group("Unsorted", unsorted));
    }
    groups
}

/// What a CAB menu entry does.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CabAction {
    /// Contents `/2` = 0, CAB slot empty, cab type 0.
    ModelCab,
    /// Cab type 1 with this VIR cab model.
    Vir(usize),
    /// Load a library item into the CAB slot (cab type 0).
    Slot(SlotChoice),
    /// Cab type 2.
    Off,
}

/// A menu entry: `action` indexes the actions list (None: a group).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MenuNode {
    pub(crate) label: String,
    pub(crate) action: Option<usize>,
    pub(crate) children: Vec<MenuNode>,
}

struct MenuBuilder {
    actions: Vec<CabAction>,
}

impl MenuBuilder {
    fn leaf(&mut self, label: impl Into<String>, action: CabAction) -> MenuNode {
        self.actions.push(action);
        MenuNode { label: label.into(), action: Some(self.actions.len() - 1), children: Vec::new() }
    }
}

fn group(label: &str, children: Vec<MenuNode>) -> MenuNode {
    MenuNode { label: label.into(), action: None, children }
}

/// The CAB menu (three levels at most) and its actions; items not named
/// yet only with `placeholders` (in "Unsorted"). The model's own cab is
/// listed by `own_cab` (the current AMP model's name).
pub(crate) fn cab_menu(catalog: &Catalog, placeholders: bool, own_cab: &str) -> (Vec<MenuNode>, Vec<CabAction>) {
    let mut b = MenuBuilder { actions: Vec::new() };
    let model_cab = b.leaf(own_cab, CabAction::ModelCab);

    // VIR ▸ size ▸ name, sizes by speaker count then size.
    let mut sizes: Vec<(String, Vec<MenuNode>)> = Vec::new();
    for (i, name) in VIR_CAB_NAMES.iter().enumerate() {
        let size = vir_size(name).unwrap_or_else(|| "Other".into());
        let leaf = b.leaf(*name, CabAction::Vir(i));
        match sizes.iter_mut().find(|(s, _)| *s == size) {
            Some((_, leaves)) => leaves.push(leaf),
            None => sizes.push((size, vec![leaf])),
        }
    }
    sizes.sort_by_key(|(s, _)| size_key(s));
    let vir = group("VIR", sizes.into_iter().map(|(s, leaves)| group(&s, leaves)).collect());

    // Hardware Lib (the pedal's library) ▸ brand ▸ model; IR ▸ name;
    // Unsorted ▸ "Cab 7".
    let mut brands: BTreeMap<String, (String, Vec<(String, LibraryId, u16)>)> = BTreeMap::new();
    let mut irs: Vec<(String, LibraryId)> = Vec::new();
    let mut unsorted: Vec<(u16, LibraryId)> = Vec::new();
    for item in &catalog.list {
        match catalog.item(&item.id) {
            Some(c) if c.is_ir() => irs.push((c.name.clone(), item.id)),
            Some(c) if matches!(c.kind, 2 | 4) => {
                let brand = brand_of(c.based_on.as_deref());
                brands.entry(brand.to_lowercase()).or_insert_with(|| (brand, Vec::new())).1.push((c.name.clone(), item.id, c.kind));
            }
            Some(_) => {} // stomp captures (kind 1): no cab
            None if placeholders => unsorted.push((item.position + 1, item.id)),
            None => {}
        }
    }
    let models = brands
        .into_values()
        .map(|(brand, mut items)| {
            items.sort_by_key(|(name, ..)| name.to_lowercase());
            let leaves = items.into_iter().map(|(name, id, kind)| b.leaf(name, CabAction::Slot(SlotChoice::Library { id, kind }))).collect();
            group(&brand, leaves)
        })
        .collect();
    irs.sort_by_key(|(name, _)| name.to_lowercase());
    let ir_leaves = irs.into_iter().map(|(name, id)| b.leaf(name, CabAction::Slot(SlotChoice::Library { id, kind: KIND_IR }))).collect();
    let unsorted_leaves: Vec<MenuNode> = unsorted
        .into_iter()
        .map(|(n, id)| b.leaf(format!("Cab {n}"), CabAction::Slot(SlotChoice::Library { id, kind: KIND_AMP_MODEL })))
        .collect();

    let mut top = vec![model_cab, vir, group("Hardware Lib", models), group("IR", ir_leaves)];
    if !unsorted_leaves.is_empty() {
        top.push(group("Unsorted", unsorted_leaves));
    }
    top.push(b.leaf("Off", CabAction::Off));
    (top, b.actions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::tests::{info, listed};

    #[test]
    fn brands_and_sizes() {
        assert_eq!(brand_of(Some("Fender Twin Reverb '65")), "Fender");
        assert_eq!(brand_of(Some("Mesa Boogie Mark IIC+")), "Mesa Boogie");
        assert_eq!(brand_of(Some("  ")), "Other");
        assert_eq!(brand_of(None), "Other");
        assert_eq!(vir_size("412 GRN").as_deref(), Some("4x12"));
        assert_eq!(vir_size("16 DK").as_deref(), Some("1x6"));
        assert_eq!(vir_size("810 SVX").as_deref(), Some("8x10"));
        assert_eq!(vir_size("GRN"), None);
        assert_eq!(vir_size("4 X"), None);
    }

    fn catalog() -> Catalog {
        let mut c = Catalog::default();
        c.set_list(listed(&[1, 2, 3, 4, 5, 6]));
        c.merge(&info(1, 4, "Polite Twin", Some("Fender Twin Reverb")), 0);
        c.merge(&info(2, 4, "Breaking Thunder", Some("Orange Thunderverb 200")), 0);
        c.merge(&info(3, 3, "Alnico 412", None), 0);
        c.merge(&info(4, 1, "Ratfish", Some("ProCo Rat")), 0);
        c.merge(&info(5, 4, "Blackface", Some("Fender Deluxe")), 0);
        c // 6: not scanned yet
    }

    #[test]
    fn amp_options_by_brand_then_name_placeholders_last() {
        let opts = amp_options(&catalog(), None, true);
        let labels: Vec<&str> = opts.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, ["Blackface", "Polite Twin", "Breaking Thunder", "Ratfish", "Amp 11"]);
        assert_eq!(opts[1].brand.as_deref(), Some("Fender"));
        // The menu groups them by brand; placeholders under "Unsorted";
        // entries point back at the (stepper's) option indices.
        let menu = amp_menu(&opts);
        let groups: Vec<&str> = menu.iter().map(|g| g.label.as_str()).collect();
        assert_eq!(groups, ["Fender", "Orange", "ProCo", "Unsorted"]);
        assert_eq!(menu[0].children.iter().map(|n| n.action).collect::<Vec<_>>(), [Some(0), Some(1)]);
        assert_eq!((menu[3].children[0].label.as_str(), menu[3].children[0].action), ("Amp 11", Some(4)));
        assert_eq!(opts[4].id, [6; 16], "unscanned items stay pickable");
        // The current item is always an option.
        let opts = amp_options(&catalog(), Some((&[9; 16], "Somewhere")), true);
        assert_eq!(opts.last().unwrap().label, "Somewhere");
        assert_eq!(amp_options(&catalog(), Some((&[1; 16], "Polite Twin")), true).len(), 5);
    }

    #[test]
    fn cab_menu_groups() {
        let (top, actions) = cab_menu(&catalog(), true, "Polite Twin");
        let labels: Vec<&str> = top.iter().map(|n| n.label.as_str()).collect();
        assert_eq!(labels, ["Polite Twin", "VIR", "Hardware Lib", "IR", "Unsorted", "Off"]);
        let action = |n: &MenuNode| actions[n.action.unwrap()].clone();
        assert_eq!(action(&top[0]), CabAction::ModelCab);
        assert_eq!(action(&top[5]), CabAction::Off);
        // VIR sizes, ordered; names under them.
        let sizes: Vec<&str> = top[1].children.iter().map(|n| n.label.as_str()).collect();
        assert_eq!(sizes, ["1x6", "1x8", "1x10", "1x12", "1x15", "2x12", "4x10", "4x12", "8x10"]);
        let x412 = &top[1].children[7];
        assert!(x412.children.iter().any(|n| n.label == "412 GRN"));
        assert_eq!(action(&x412.children[0]), CabAction::Vir(17), "412 1960");
        // Hardware Lib by brand (amp captures only), IRs, unscanned.
        let brands: Vec<&str> = top[2].children.iter().map(|n| n.label.as_str()).collect();
        assert_eq!(brands, ["Fender", "Orange"]);
        let fender: Vec<&str> = top[2].children[0].children.iter().map(|n| n.label.as_str()).collect();
        assert_eq!(fender, ["Blackface", "Polite Twin"]);
        assert_eq!(action(&top[3].children[0]), CabAction::Slot(SlotChoice::Library { id: [3; 16], kind: KIND_IR }));
        assert_eq!(top[4].children[0].label, "Cab 11");
        assert_eq!(action(&top[4].children[0]), CabAction::Slot(SlotChoice::Library { id: [6; 16], kind: KIND_AMP_MODEL }));
        // Nothing unscanned: no Unsorted group.
        let mut c = catalog();
        c.merge(&info(6, 4, "X", None), 0);
        assert!(!cab_menu(&c, true, "X").0.iter().any(|n| n.label == "Unsorted"));
    }

    /// After a completed scan, items the pedal never named are hidden
    /// (no dead "Amp N"); UTF-8 names sort and show like any other.
    #[test]
    fn unnamed_hidden_after_completed_scan() {
        let mut c = catalog();
        c.merge(&info(7, 4, "Plæxi Driver", Some("Marshall Super Lead MKII")), 0);
        c.set_list(listed(&[1, 2, 3, 4, 5, 6, 7]));
        let labels = |p: bool| amp_options(&c, None, p).into_iter().map(|o| o.label).collect::<Vec<_>>();
        assert_eq!(labels(true), ["Blackface", "Polite Twin", "Plæxi Driver", "Breaking Thunder", "Ratfish", "Amp 11"]);
        assert_eq!(labels(false), ["Blackface", "Polite Twin", "Plæxi Driver", "Breaking Thunder", "Ratfish"]);
        assert!(!cab_menu(&c, false, "X").0.iter().any(|n| n.label == "Unsorted"));
        let (top, _) = cab_menu(&c, false, "X");
        let marshall = top[2].children.iter().find(|b| b.label == "Marshall").unwrap();
        assert_eq!(marshall.children[0].label, "Plæxi Driver");
    }
}
