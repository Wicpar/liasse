//! Evaluation of the `blob` descriptor member selectors `.$sha512`, `.$bytes`,
//! `.$media`, `.$name` (§18.1) and the placement member selectors `.$satisfied`,
//! `.$stored`, `.$surplus` (§18.5).
//!
//! The base evaluates to a `blob` descriptor value. A metadata member is read
//! off it, canonicalised as the checker typed it: the content hash as
//! lowercase-hex `text`, the byte count as `int`, the media type as `text`, and
//! the optional file name as `text` or `none`. A placement member defers to the
//! environment's placement index ([`Environment::blob_placement`]): `$satisfied`
//! is the recorded policy-satisfaction `bool`, and `$stored`/`$surplus` are the
//! recorded store identities returned as a view of keyed store-identity rows.
//!
//! [`Environment::blob_placement`]: crate::Environment::blob_placement

use liasse_value::{Integer, Text, Value};

use crate::env::{Cell, Row, RowId};
use crate::error::EvalError;
use crate::eval::Evaluator;
use crate::typed::{BlobMember, TypedExpr};

impl Evaluator<'_> {
    pub(crate) fn eval_blob_member(
        &mut self,
        base: &TypedExpr,
        member: BlobMember,
    ) -> Result<Cell, EvalError> {
        let descriptor = match self.eval(base)? {
            Cell::Scalar(Value::Blob(descriptor)) => descriptor,
            _ => return Err(EvalError::ShapeMismatch { expected: "a blob descriptor" }),
        };
        let value = match member {
            BlobMember::Sha512 => Value::Text(Text::new(descriptor.sha512().to_canonical_text())),
            BlobMember::Bytes => Value::Int(Integer::from(descriptor.byte_count() as i64)),
            BlobMember::Media => Value::Text(Text::new(descriptor.media().as_str().to_owned())),
            BlobMember::Name => match descriptor.name() {
                Some(name) => Value::Text(Text::new(name.to_owned())),
                None => Value::None,
            },
            // §18.5: placement observations are engine-recorded, so the evaluator
            // reads them off the environment's placement index rather than
            // computing physical placement itself.
            BlobMember::Satisfied => {
                Value::Bool(self.env.blob_placement(descriptor.as_ref())?.satisfied)
            }
            BlobMember::Stored => {
                let placement = self.env.blob_placement(descriptor.as_ref())?;
                return Ok(Cell::Collection(store_identity_rows(placement.stored)));
            }
            BlobMember::Surplus => {
                let placement = self.env.blob_placement(descriptor.as_ref())?;
                return Ok(Cell::Collection(store_identity_rows(placement.surplus)));
            }
        };
        Ok(Cell::Scalar(value))
    }
}

/// Turn a set of §18.5 store identities into the store-identity rows a
/// `$stored`/`$surplus` view yields: each row is keyed by the store id and
/// carries it as its one `id` `text` cell, so the view projects as `{ id }` and a
/// `/stores['id']` row tests membership by that stable key (§12.4, §18.11).
fn store_identity_rows(ids: Vec<Text>) -> Vec<Row> {
    ids.into_iter()
        .map(|id| {
            let key = Value::Text(id.clone());
            Row::new(
                RowId::keyed(id.as_str().to_owned()),
                key,
                [("id".to_owned(), Cell::Scalar(Value::Text(id)))],
            )
        })
        .collect()
}
