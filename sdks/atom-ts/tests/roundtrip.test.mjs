import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import assert from "node:assert/strict";

// spec: [CF-3]

const here = dirname(fileURLToPath(import.meta.url));
const dir = join(here, "..", "..", "..", "docs", "specs", "schemas", "atom-1");

function checkType(v, t) {
  switch (t) {
    case "object":
      return v !== null && typeof v === "object" && !Array.isArray(v);
    case "array":
      return Array.isArray(v);
    case "string":
      return typeof v === "string";
    case "integer":
      return typeof v === "number" && Number.isInteger(v);
    case "number":
      return typeof v === "number";
    case "boolean":
      return typeof v === "boolean";
    case "null":
      return v === null;
    default:
      return true;
  }
}

// Validador mínimo p/ o subconjunto usado pelos schemas atom-1:
// type (string ou lista), required, properties, minimum/maximum, enum.
function validate(schema, instance, path, errors) {
  if (schema === true || schema === undefined) return;
  if (schema === false) {
    errors.push(`${path}: false schema`);
    return;
  }
  if (typeof schema !== "object" || schema === null) return;
  if (schema.type !== undefined) {
    const types = Array.isArray(schema.type) ? schema.type : [schema.type];
    if (!types.some((t) => checkType(instance, t))) {
      errors.push(`${path}: esperado ${JSON.stringify(schema.type)}`);
      return;
    }
  }
  if (schema.enum !== undefined && !schema.enum.includes(instance)) {
    errors.push(`${path}: fora do enum`);
  }
  if (typeof instance === "number") {
    if (schema.minimum !== undefined && instance < schema.minimum)
      errors.push(`${path}: < minimum ${schema.minimum}`);
    if (schema.maximum !== undefined && instance > schema.maximum)
      errors.push(`${path}: > maximum ${schema.maximum}`);
  }
  if (schema.required !== undefined && instance !== null && typeof instance === "object" && !Array.isArray(instance)) {
    for (const k of schema.required) {
      if (!(k in instance)) errors.push(`${path}: falta required "${k}"`);
    }
  }
  if (schema.properties !== undefined && instance !== null && typeof instance === "object" && !Array.isArray(instance)) {
    for (const [k, sub] of Object.entries(schema.properties)) {
      if (k in instance) validate(sub, instance[k], `${path}.${k}`, errors);
    }
  }
  if (schema.items !== undefined && Array.isArray(instance) && typeof schema.items === "object") {
    instance.forEach((v, i) => validate(schema.items, v, `${path}[${i}]`, errors));
  }
}

const files = readdirSync(dir).filter((f) => f.endsWith(".json")).sort();

for (const file of files) {
  test(`schema ${file}: exemplos fazem round-trip e validam`, () => {
    const raw = readFileSync(join(dir, file), "utf8");
    const schema = JSON.parse(raw);
    const examples = schema.examples ?? [];
    assert.ok(examples.length > 0, `${file} sem examples (exige [CF-3])`);
    for (let i = 0; i < examples.length; i++) {
      const ex = examples[i];
      const what = `${file} exemplo ${i}`;
      assert.ok("params" in ex, `${what} sem params`);
      // Round-trip: desserializa/reserializa sem perda.
      const back = JSON.parse(JSON.stringify(ex.params));
      assert.deepEqual(back, ex.params, `${what}: round-trip com perda`);
      const errors = [];
      validate(schema, back, `${what} params`, errors);
      assert.deepEqual(errors, [], `${what}: params inválido: ${errors.join("; ")}`);
      // Result valida no subschema `result` quando ele é um schema tipado
      // (mesma regra do teste Rust em crates/atom-protocol/tests/schemas.rs).
      if (ex.result !== undefined && schema.result && typeof schema.result === "object" && schema.result.type !== undefined) {
        const rerrs = [];
        validate(schema.result, ex.result, `${what} result`, rerrs);
        assert.deepEqual(rerrs, [], `${what}: result inválido: ${rerrs.join("; ")}`);
      }
    }
  });
}
