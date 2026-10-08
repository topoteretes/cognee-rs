/**
 * Types that can be passed as pipeline values. Objects (including arrays) are
 * carried by reference and reach later JS tasks as the same object; `null`
 * and `undefined` are rejected.
 */
export type CogneeValue = number | boolean | string | Buffer | object;
