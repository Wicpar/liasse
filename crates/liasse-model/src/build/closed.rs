//! Closed declaration-object vocabularies (SPEC.md §2.5, Annex C.2/C.3).
//!
//! §2.5: "Unknown members in a declaration object are invalid unless that
//! declaration explicitly accepts application-defined member names."
//!
//! A node builder reads the members its form defines and returns the node. What
//! it does *not* read used to be dropped in silence, so a declaration could load
//! meaning something other than what it says: `{ "$set": "text", "$check": [...] }`
//! built a set and discarded the membership constraint the package plainly
//! intends to enforce — no diagnostic, no constraint, in a keyed row and at the
//! model root alike. The same hole sat under `$view`, `$ref`, an inline `$enum`,
//! `$like`, and a source-backed `$bucket` collection.
//!
//! Each form therefore states exactly what its builder consumes, and
//! [`closed_decl`] rejects every other member **by name**. The vocabularies below
//! are descriptions of the builders, not new policy: nothing here widens or
//! narrows what a builder accepts, it only makes what a builder ignores loud.

use liasse_syntax::DocValue;

use crate::doc::DocValueExt;
use crate::report::{code, Reporter};

/// The closed member vocabulary of one object-form declaration (§2.5).
pub(super) struct DeclForm {
    /// The marker that names the form, for the diagnostic.
    kind: &'static str,
    /// Every member the form's builder actually reads.
    accepted: &'static [&'static str],
    /// Whether the form is one of §2.5's declarations that "explicitly accepts
    /// application-defined member names". True only for a source-backed bucket,
    /// whose non-`$` members are its output-field expressions (§14.4).
    app_named: bool,
    /// Where the refinement the author probably wanted actually belongs.
    hint: &'static str,
}

/// `{ "$set": <element shape> }` (§5.5). The builder reads the element shape and
/// nothing else: a set node carries an element type and an optional member ref,
/// so it has no place to put a check, a default, or a normalization.
pub(super) const SET: DeclForm = DeclForm {
    kind: "$set",
    accepted: &["$set"],
    app_named: false,
    hint: "a constraint over the membership belongs on the CONTAINING shape's `$check` (§5.10) — \
           e.g. `\"$check\": [\"size(.flags) <= 3\", \"at most three flags\"]` beside the set, not inside it",
};

/// `{ "$view": "<expression>" }` (§7). Everything a view declares — its source
/// selection, filters, projection, `$sort`, `$skip`/`$limit` — is written inside
/// the expression text (Annex C.7), not as a sibling member.
pub(super) const VIEW: DeclForm = DeclForm {
    kind: "$view",
    accepted: &["$view"],
    app_named: false,
    hint: "a view declares its projection, `$sort`, and bounds INSIDE the `$view` expression \
           (§7.2, §7.3, C.7) — e.g. `\"$view\": \".docs { id, title, $sort: [-created_at] }\"`",
};

/// `{ "$ref": "<target>", "$optional": bool, "$on_delete": "<program>" }`
/// (§5.6, §21.1). The three members the ref builder reads are the whole form.
pub(super) const REF: DeclForm = DeclForm {
    kind: "$ref",
    accepted: &["$ref", "$optional", "$on_delete"],
    app_named: false,
    hint: "a ref field is refined by `$optional` and `$on_delete` only; a check over the \
           referencing row belongs on the containing shape's `$check` (§5.10)",
};

/// A bare inline `{ "$enum": [labels] }` (§5.9). An enum that also carries an
/// expanded-field refinement is an expanded field and is built — and closed —
/// by [`super::Builder::expanded_field`] instead, so anything reaching this form
/// is outside both vocabularies.
pub(super) const ENUM: DeclForm = DeclForm {
    kind: "$enum",
    accepted: &["$enum"],
    app_named: false,
    hint: "to refine an inline enum, expand the field: `$default`, `$optional`, `$normalize`, \
           `$check`, `$unique` beside the `$enum` make it an expanded field (§5.1, §5.9)",
};

/// `{ "$like": "^" }` positional recursion (§5.8). The field adopts the named
/// shape's contract whole, so it has no refinements of its own.
pub(super) const LIKE: DeclForm = DeclForm {
    kind: "$like",
    accepted: &["$like"],
    app_named: false,
    hint: "a `$like` field adopts the named shape's contract whole; refine the shape it names, \
           or spell the field out instead of adopting one",
};

/// A `$keyring` managed-keyring declaration (§17.1, C.16): the ring's policy is
/// the `$keyring` value, and nothing may sit beside it.
pub(super) const KEYRING: DeclForm = DeclForm {
    kind: "$keyring",
    accepted: &["$keyring"],
    app_named: false,
    hint: "declare the ring's policy inside `$keyring` — `$provider`, `$algorithm`, `$usage`, \
           `$rotate`, `$retain`, `$protection` (C.16)",
};

/// A source-backed bucket collection (§14.4–§14.6): the `$bucket` source
/// declaration, an optional custom `$key` built from its structural bindings,
/// and the application-named output-field expressions
/// ([`crate::bucket::type_source_buckets`] reads exactly these). Its rows are
/// derived and read-only, so the row-level declarations an ordinary collection
/// carries have nothing to act on here.
pub(super) const SOURCE_BUCKET: DeclForm = DeclForm {
    kind: "$bucket",
    accepted: &["$bucket", "$key"],
    app_named: true,
    hint: "a source-backed bucket derives read-only rows (§14.4): it declares `$bucket`, an \
           optional `$key`, and its output fields — expose or constrain it through the view \
           that reads it",
};

/// Reject every member of a closed declaration object that its builder does not
/// read (§2.5), naming the member and the form's own vocabulary.
///
/// `reported` lists the members a caller has already rejected by name — the
/// mutually-exclusive shape markers and the misplaced `$value` that
/// [`super::Builder::object_node`] judges before it dispatches — so one mistake
/// yields one diagnostic rather than two.
pub(super) fn closed_decl(
    reporter: &mut Reporter,
    value: &DocValue,
    reported: &[&str],
    form: &DeclForm,
) {
    for member in value.as_object().unwrap_or(&[]) {
        let name = member.name.text.as_str();
        if form.accepted.contains(&name) || reported.contains(&name) {
            continue;
        }
        if form.app_named && !name.starts_with('$') {
            continue;
        }
        reporter.reject_hint(
            member.span,
            code::UNKNOWN_MEMBER,
            format!(
                "`{name}` may not accompany a `{}` declaration, whose members are {} (§2.5)",
                form.kind,
                form.accepted
                    .iter()
                    .map(|m| format!("`{m}`"))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            form.hint,
        );
    }
}
