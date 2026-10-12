use std::iter::{once, successors};

// use std::iter::successors;
use stdx::impl_display;
// use stdx::impl_display;
use syntax::ast;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Err,
    Real,
    Integer,
    Bool,
    String,
    Array { ty: Box<Type>, len: u32 },
    EmptyArray,
    Void,
}

use Type::*;
impl_display! {
    match Type{
        Err => "[missing]";
        Real => "real";
        Integer => "integer";
        Bool => "integer";
        Void => "void";
        String => "string";
        EmptyArray => "_[0:0]";
        // An array says its dimensions in the order they are declared in,
        // outermost first, which the nesting of the type runs the other way round
        // from -- hence a function rather than a format string.
        Array{ty,len} => "{}",array_dims(ty,*len);
    }
}

/// An array type's shape, e.g. `real[0:1][0:2]` for `real m[0:1][0:2]`.
///
/// A declared range may start anywhere (`real g[2:5]`), but a type knows only how
/// many elements each of its dimensions has, so it says so from zero.
fn array_dims(elem: &Type, len: u32) -> std::string::String {
    let mut res = elem.base_type().to_string();
    for len in once(len).chain(elem.dim()) {
        res.push_str(&format!("[0:{}]", len.saturating_sub(1)));
    }
    res
}

impl Type {
    pub fn is_numeric(&self) -> bool {
        matches!(self, Type::Real | Type::Integer | Type::Bool)
    }

    pub fn is_convertible_to(&self, dst: &Type) -> bool {
        match (dst, self) {
            (Type::Real, Type::Integer | Type::Bool)
            | (Type::Integer, Type::Bool)
            | (Type::Bool, Type::Integer)
            | (Type::Err, _)
            | (_, Type::Err)
            | (Type::EmptyArray, Type::Array { len: 0, .. })
            | (Type::Array { len: 0, .. }, Type::EmptyArray) => true,

            (Type::Array { .. }, Type::Array { .. }) => {
                self.dim() == dst.dim() && self.base_type().is_convertible_to(dst.base_type())
            }
            _ => dst == self,
        }
    }

    pub fn is_semantically_equivalent(&self, dst: &Type) -> bool {
        match (dst, self) {
            (Type::Integer, Type::Bool)
            | (Type::EmptyArray, Type::Array { len: 0, .. })
            | (Type::Array { len: 0, .. }, Type::EmptyArray)
            | (Type::Bool, Type::Integer) => true,

            (Type::Array { .. }, Type::Array { .. }) => {
                self.dim() == dst.dim()
                    && self.base_type().is_semantically_equivalent(dst.base_type())
            }

            _ => dst == self,
        }
    }

    pub fn union(&self, other: &Type) -> Option<Type> {
        match (self, other) {
            (Type::Real, Type::Integer | Type::Bool) | (Type::Integer | Type::Bool, Type::Real) => {
                Some(Type::Real)
            }
            (Type::Bool, Type::Integer) | (Type::Integer, Type::Bool) => Some(Type::Integer),

            (
                Type::EmptyArray | Type::Array { len: 0, .. },
                Type::EmptyArray | Type::Array { len: 0, .. },
            ) => Some(Type::EmptyArray),

            _ if self == other => Some(self.clone()),
            _ if self.dim() == other.dim() => {
                let base_type1 = self.base_type();
                let base_type2 = self.base_type();
                let ty = base_type1.union(base_type2)?;
                debug_assert_ne!(self.dim(), &[]);
                Some(ty.to_dim(&self.dim()))
            }
            _ => None,
        }
    }

    pub fn base_type(&self) -> &Type {
        let mut curr = self;
        while let Type::Array { ty, .. } = curr {
            curr = ty
        }
        curr
    }

    /// This type with `dim`'s dimensions wrapped around it, outermost first.
    #[must_use]
    pub fn to_dim(self, dim: &[u32]) -> Type {
        let mut ty = self;
        for len in dim.iter().rev().copied() {
            ty = Type::Array { ty: Box::new(ty), len };
        }
        ty
    }

    /// The element count of each dimension, outermost first: the order they are
    /// declared in, and the order [`Self::to_dim`] reads them back.
    pub fn dim(&self) -> Vec<u32> {
        if let Type::Array { ref ty, len } = *self {
            successors(Some((ty, len)), |(ty, _)| {
                if let Type::Array { ref ty, len } = ***ty {
                    Some((ty, len))
                } else {
                    None
                }
            })
            .map(|(_, len)| len)
            .collect()
        } else {
            vec![]
        }
    }

    pub fn is_assignable_to(&self, dst: &Type) -> bool {
        self.is_convertible_to(dst)
            || (self.base_type().is_numeric()
                && dst.base_type().is_numeric()
                && self.dim() == dst.dim())
    }
}

pub trait AsType {
    fn as_type(&self) -> Type;
}

impl AsType for ast::Type {
    fn as_type(&self) -> Type {
        if self.integer_token().is_some() {
            Type::Integer
        } else if self.real_token().is_some() {
            Type::Real
        } else if self.string_token().is_some() {
            Type::String
        } else {
            Type::Err
        }
    }
}

impl AsType for Option<ast::Type> {
    fn as_type(&self) -> Type {
        self.as_ref().map_or(Type::Err, AsType::as_type)
    }
}
