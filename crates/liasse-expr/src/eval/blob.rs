//! Evaluation of the `blob` descriptor member selectors `.$sha512`, `.$bytes`,
//! `.$media`, `.$name` (§18.1).
//!
//! The base evaluates to a `blob` descriptor value; each selector reads one
//! member off it, canonicalised as the checker typed it: the content hash as
//! lowercase-hex `text`, the byte count as `int`, the media type as `text`, and
//! the optional file name as `text` or `none`.

use liasse_value::{Integer, Text, Value};

use crate::env::Cell;
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
        };
        Ok(Cell::Scalar(value))
    }
}
