import { writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { protocolJsonSchema } from "../src/protocol.ts";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
writeFileSync(resolve(root, "protocol.schema.json"), `${JSON.stringify(protocolJsonSchema(), null, 2)}\n`);
