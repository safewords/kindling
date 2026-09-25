import { reactive } from "vue";
import { api } from "./api.js";

/**
 * What a condition can ask, as the server describes it.
 *
 * Fetched from `/api/policy/schema` rather than written out here: the server
 * is where facts and operators are defined and compiled, so a fact added there
 * appears in every editor without this file changing. Loaded once and shared.
 */
export const schema = reactive({ loaded: false, facts: [], operators: {}, profile_kinds: [], placeholders: [], architectures: [] });

let loading = null;

export function loadSchema() {
  if (!loading) {
    loading = api.policySchema().then((s) => {
      Object.assign(schema, s, { loaded: true });
      return schema;
    });
    loading.catch(() => (loading = null));
  }
  return loading;
}

export const factSpec = (name) => schema.facts.find((f) => f.name === name);
export const opSpec = (op) => schema.operators[op] || { label: op, value: "", takes_value: true };

/** Facts grouped for a picker, in the server's order. */
export function factGroups() {
  const groups = [];
  for (const fact of schema.facts) {
    let group = groups.find((g) => g.name === fact.group);
    if (!group) groups.push((group = { name: fact.group, facts: [] }));
    group.facts.push(fact);
  }
  return groups;
}

/** Which kind of editor a value needs. */
export function valueKind(fact, op) {
  const spec = factSpec(fact);
  if (!spec || !opSpec(op).takes_value) return "none";
  if (spec.type === "bool") return "bool";
  if (spec.type === "number") return op === "between" ? "range" : "number";
  if (spec.type === "time") return "window";
  if (spec.type === "choice" || spec.type === "weekday") {
    return op === "equals" || op === "not_equals" ? "choice" : "choices";
  }
  return op === "equals" || op === "not_equals" ? "text" : "list";
}

/** A sensible starting value for a fact and operator. */
export function defaultValue(fact, op) {
  const spec = factSpec(fact);
  switch (valueKind(fact, op)) {
    case "none":
      return null;
    case "bool":
      return spec?.example === "false" ? false : true;
    case "number":
      return Number(spec?.example) || 0;
    case "range":
      return [0, 3];
    case "window":
      return ["22:00", "06:00"];
    case "choice":
      return spec?.choices?.[0] || "";
    case "choices":
      return [];
    case "text":
      return "";
    default:
      return [];
  }
}

export function newTest(fact = "vendor") {
  const spec = factSpec(fact);
  const op = spec?.operators?.[0] || "glob";
  const test = { fact, op, value: defaultValue(fact, op) };
  if (spec?.keyed) test.key = "";
  return test;
}

export const isGroup = (node) => node && ("all" in node || "any" in node || "not" in node);
export const always = () => ({ all: [] });

/** A condition tree in words — the same shape of sentence the server writes. */
export function describe(node) {
  if (!node) return "anything";
  if ("all" in node) {
    if (!node.all.length) return "anything";
    return node.all.map((c) => wrap(c, "all")).join(" and ");
  }
  if ("any" in node) {
    if (!node.any.length) return "nothing (an empty “any”)";
    return node.any.map((c) => wrap(c, "any")).join(" or ");
  }
  if ("not" in node) return `not ${wrap(node.not, "not")}`;

  const spec = factSpec(node.fact);
  const fact = node.key ? `${spec?.label || node.fact} “${node.key}”` : spec?.label || node.fact;
  const op = opSpec(node.op);
  if (!op.takes_value) return `${fact} ${op.label}`;
  return `${fact} ${op.label} ${showValue(node.value)}`;
}

function wrap(node, parent) {
  const text = describe(node);
  if (isGroup(node) && !("not" in node) && Object.keys(node)[0] !== parent) {
    const children = node.all || node.any;
    if (children?.length > 1) return `(${text})`;
  }
  return text;
}

export function showValue(value) {
  if (Array.isArray(value)) return value.length ? value.join(" | ") : "…";
  if (value === true) return "yes";
  if (value === false) return "no";
  if (value === "" || value === null || value === undefined) return "…";
  return String(value);
}

/** Every test in a tree, flattened — for "which facts does this ask". */
export function tests(node, out = []) {
  if (!node) return out;
  if ("all" in node) node.all.forEach((c) => tests(c, out));
  else if ("any" in node) node.any.forEach((c) => tests(c, out));
  else if ("not" in node) tests(node.not, out);
  else out.push(node);
  return out;
}

/** Tidy a tree for saving: collapse groups of one, drop empty values. */
export function normalise(node) {
  if (!node) return always();
  if ("all" in node || "any" in node) {
    const key = "all" in node ? "all" : "any";
    const children = node[key].map(normalise).filter((c) => !(("all" in c) && !c.all.length && key === "all"));
    return { [key]: children };
  }
  if ("not" in node) return { not: normalise(node.not) };
  const test = { fact: node.fact, op: node.op };
  if (node.key !== undefined && node.key !== null) test.key = String(node.key).trim();
  if (opSpec(node.op).takes_value) {
    let value = node.value;
    if (Array.isArray(value)) value = value.map((v) => (typeof v === "string" ? v.trim() : v)).filter((v) => v !== "");
    test.value = value;
  }
  return test;
}

export const clone = (value) => JSON.parse(JSON.stringify(value ?? null));
