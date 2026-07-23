//! Definition-time collection of accepted blob fields and placement policies.

use liasse_diag::SourceMap;
use liasse_expr::ExprType;
use liasse_syntax::DocValue;
use liasse_value::MediaType;

use crate::blobs::AcceptedType;
use crate::doc;
use crate::error::EngineError;
use crate::schema::Schema;
use crate::scope::RuntimeScope;

use super::{CompiledBlobField, CompiledPlacement, CompiledPolicy};

pub(super) fn compile(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root: &ExprType,
    model: &DocValue,
) -> Result<Vec<CompiledBlobField>, EngineError> {
    let mut fields = Vec::new();
    BlobCompiler {
        sources,
        schema,
        root,
        fields: &mut fields,
    }
    .walk_shape(model, &[], None)?;
    Ok(fields)
}

struct BlobCompiler<'a, 'm> {
    sources: &'a mut SourceMap,
    schema: Schema<'m>,
    root: &'a ExprType,
    fields: &'a mut Vec<CompiledBlobField>,
}

impl BlobCompiler<'_, '_> {
    fn walk_shape(
        &mut self,
        shape: &DocValue,
        path: &[String],
        inherited: Option<CompiledPolicy>,
    ) -> Result<(), EngineError> {
        let policy = match doc::member(shape, "$blob_storage") {
            Some(storage) => Some(self.compile_policy(storage, path)?),
            None => inherited,
        };
        for member in doc::object(shape).unwrap_or_default() {
            if member.name.text.starts_with('$') {
                continue;
            }
            let mut member_path = path.to_vec();
            member_path.push(member.name.text.clone());
            if is_blob_field(&member.value) {
                if let (Some(accepted), Some(policy)) =
                    (accepted_type(&member.value), policy.clone())
                {
                    self.fields.push(CompiledBlobField {
                        path: member_path,
                        accepted,
                        policy,
                    });
                }
            } else if is_shape(&member.value) {
                self.walk_shape(&member.value, &member_path, policy.clone())?;
            }
        }
        Ok(())
    }

    fn compile_policy(
        &mut self,
        storage: &DocValue,
        path: &[String],
    ) -> Result<CompiledPolicy, EngineError> {
        let input = doc::member(storage, "$in").ok_or_else(|| {
            EngineError::Internal("validated `$blob_storage` lost its `$in` member".to_owned())
        })?;
        let plan = self.compile_placement(input, path)?;
        let serve = doc::member(storage, "$serve")
            .map(|value| self.compile_view(value, path))
            .transpose()?;
        Ok(CompiledPolicy { plan, serve })
    }

    fn compile_placement(
        &mut self,
        value: &DocValue,
        path: &[String],
    ) -> Result<CompiledPlacement, EngineError> {
        if doc::string(value).is_some() {
            return self.compile_view(value, path).map(CompiledPlacement::View);
        }
        if let Some(items) = doc::member(value, "$all").and_then(doc::array) {
            return items
                .iter()
                .map(|item| self.compile_placement(item, path))
                .collect::<Result<Vec<_>, _>>()
                .map(CompiledPlacement::All);
        }
        if let Some(items) = doc::member(value, "$any").and_then(doc::array) {
            return items
                .iter()
                .map(|item| self.compile_placement(item, path))
                .collect::<Result<Vec<_>, _>>()
                .map(CompiledPlacement::Any);
        }
        let n = doc::member(value, "$copies")
            .and_then(unsigned)
            .and_then(|count| usize::try_from(count).ok())
            .ok_or_else(|| {
                EngineError::Internal(
                    "validated `$copies` placement lost its positive count".to_owned(),
                )
            })?;
        let of = doc::member(value, "$of").ok_or_else(|| {
            EngineError::Internal("validated `$copies` placement lost `$of`".to_owned())
        })?;
        Ok(CompiledPlacement::Copies {
            n,
            of: self.compile_view(of, path)?,
        })
    }

    fn compile_view(
        &mut self,
        value: &DocValue,
        path: &[String],
    ) -> Result<liasse_expr::TypedExpr, EngineError> {
        let text = doc::string(value).ok_or_else(|| {
            EngineError::Internal("validated blob store view is not text".to_owned())
        })?;
        if !text.trim_start().starts_with('/') {
            return Err(EngineError::Unsupported(format!(
                "relative `$blob_storage` store view `{text}` at `{}` is not yet \
                 resolvable without the uploaded occurrence context; use an absolute \
                 store view until occurrence-scoped placement lands",
                path.join(".")
            )));
        }
        let contexts = self.schema.context_chain(path);
        let scope = RuntimeScope::nested(contexts, self.root.clone());
        super::super::compile_expr(self.sources, &scope, "blob-storage", text)
            .map(|(typed, _)| typed)
    }
}

fn is_blob_field(value: &DocValue) -> bool {
    doc::member(value, "$type")
        .and_then(doc::string)
        .is_some_and(|ty| ty.trim() == "blob")
}

fn is_shape(value: &DocValue) -> bool {
    let Some(members) = doc::object(value) else {
        return false;
    };
    !members.iter().any(|member| {
        matches!(
            member.name.text.as_str(),
            "$type" | "$enum" | "$set" | "$ref" | "$view" | "$like" | "$keyring" | "$modules"
        )
    })
}

fn accepted_type(value: &DocValue) -> Option<AcceptedType> {
    let max_bytes = doc::member(value, "$max_bytes").and_then(unsigned)?;
    let media = doc::member(value, "$media")
        .and_then(doc::array)?
        .iter()
        .filter_map(doc::string)
        .map(MediaType::new)
        .collect::<Vec<_>>();
    (!media.is_empty()).then_some(AcceptedType { max_bytes, media })
}

fn unsigned(value: &DocValue) -> Option<u64> {
    match doc::to_json(value) {
        serde_json::Value::String(text) => text.parse().ok(),
        serde_json::Value::Number(number) => number.as_u64(),
        _ => None,
    }
}
