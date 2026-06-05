import { existsSync } from "node:fs";
import { resolve } from "node:path";
import dotenv from "dotenv";

const disabled = process.env.COGNEE_DISABLE_DOTENV;
if (disabled === "1" || disabled === "true") {
  // Opt out explicitly for hosts that manage env loading themselves.
} else {
  const cwd = process.cwd();
  const candidates = [resolve(cwd, ".env"), resolve(cwd, "..", ".env")];

  for (const file of candidates) {
    if (!existsSync(file)) {
      continue;
    }

    dotenv.config({ path: file });
    break;
  }
}
