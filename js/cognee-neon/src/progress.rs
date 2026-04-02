use neon::prelude::*;

use cognee_core::ProgressToken;

use crate::error::throw_core_error;

pub struct NeonProgressToken {
    pub inner: ProgressToken,
}

impl Finalize for NeonProgressToken {}

pub fn progress_new(mut cx: FunctionContext) -> JsResult<JsBox<NeonProgressToken>> {
    Ok(cx.boxed(NeonProgressToken {
        inner: ProgressToken::new(),
    }))
}

pub fn progress_set(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let token = cx.argument::<JsBox<NeonProgressToken>>(0)?;
    let fraction = cx.argument::<JsNumber>(1)?.value(&mut cx);
    token.inner.set(fraction);
    Ok(cx.undefined())
}

pub fn progress_fraction(mut cx: FunctionContext) -> JsResult<JsNumber> {
    let token = cx.argument::<JsBox<NeonProgressToken>>(0)?;
    Ok(cx.number(token.inner.fraction()))
}

pub fn progress_width(mut cx: FunctionContext) -> JsResult<JsNumber> {
    let token = cx.argument::<JsBox<NeonProgressToken>>(0)?;
    Ok(cx.number(token.inner.width()))
}

pub fn progress_is_complete(mut cx: FunctionContext) -> JsResult<JsBoolean> {
    let token = cx.argument::<JsBox<NeonProgressToken>>(0)?;
    Ok(cx.boolean(token.inner.is_complete()))
}

pub fn progress_root_fraction(mut cx: FunctionContext) -> JsResult<JsNumber> {
    let token = cx.argument::<JsBox<NeonProgressToken>>(0)?;
    Ok(cx.number(token.inner.root_fraction()))
}

/// Split into subtokens by relative weights.
///
/// `progressSplit(token, weights: number[]) -> ProgressToken[]`
pub fn progress_split(mut cx: FunctionContext) -> JsResult<JsArray> {
    let token = cx.argument::<JsBox<NeonProgressToken>>(0)?;
    let weights_arr = cx.argument::<JsArray>(1)?;
    let len = weights_arr.len(&mut cx);
    let mut weights = Vec::with_capacity(len as usize);
    for i in 0..len {
        let w = weights_arr
            .get::<JsNumber, _, _>(&mut cx, i)?
            .value(&mut cx) as u32;
        weights.push(w);
    }

    let subtokens = token
        .inner
        .split(&weights)
        .or_else(|e| throw_core_error(&mut cx, e))?;

    let result = JsArray::new(&mut cx, subtokens.len());
    for (i, sub) in subtokens.into_iter().enumerate() {
        let js_sub = cx.boxed(NeonProgressToken { inner: sub });
        result.set(&mut cx, i as u32, js_sub)?;
    }
    Ok(result)
}

/// Create one child subtoken covering `fracWidth` of this token's range.
///
/// `progressSubtoken(token, fracWidth: number) -> ProgressToken`
pub fn progress_subtoken(mut cx: FunctionContext) -> JsResult<JsBox<NeonProgressToken>> {
    let token = cx.argument::<JsBox<NeonProgressToken>>(0)?;
    let frac_width = cx.argument::<JsNumber>(1)?.value(&mut cx);
    let sub = token.inner.subtoken(frac_width);
    Ok(cx.boxed(NeonProgressToken { inner: sub }))
}

pub fn progress_clone(mut cx: FunctionContext) -> JsResult<JsBox<NeonProgressToken>> {
    let token = cx.argument::<JsBox<NeonProgressToken>>(0)?;
    Ok(cx.boxed(NeonProgressToken {
        inner: token.inner.clone(),
    }))
}
