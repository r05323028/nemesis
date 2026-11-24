// NEMESIS-MANAGED: nemesis-review-extension@0.1.0
// Official Pi Extension API integration. The Rust CLI owns reviewer execution and persistence.

import { spawn } from "node:child_process";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type, type Static } from "typebox";

const Parameters = Type.Object({
  operation: Type.Union([
    Type.Literal("run"),
    Type.Literal("status"),
    Type.Literal("events"),
    Type.Literal("wait"),
    Type.Literal("result"),
    Type.Literal("cancel"),
    Type.Literal("verify"),
  ]),
  prompt: Type.Optional(Type.String({ description: "Review request for an independent reviewer" })),
  run_id: Type.Optional(Type.String({ description: "NEMESIS review or originating review run identifier" })),
  finding_id: Type.Optional(Type.String({ description: "Finding identifier for independent verification" })),
  tail: Type.Optional(Type.Integer({ minimum: 1 })),
  timeout_seconds: Type.Optional(Type.Integer({ minimum: 1 })),
});

type Parameters = Static<typeof Parameters>;

type CommandResult = {
  stdout: string;
  stderr: string;
  exitCode: number;
};

function runNemesis(args: string[], signal?: AbortSignal): Promise<CommandResult> {
  const executable = process.env.NEMESIS_BIN ?? "nemesis";
  return new Promise((resolve, reject) => {
    const child = spawn(executable, ["--json", ...args], {
      cwd: process.cwd(),
      env: process.env,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk: Buffer) => {
      stdout += chunk.toString();
    });
    child.stderr.on("data", (chunk: Buffer) => {
      stderr += chunk.toString();
    });
    const abort = () => child.kill("SIGTERM");
    signal?.addEventListener("abort", abort, { once: true });
    child.once("error", (error) => {
      signal?.removeEventListener("abort", abort);
      reject(error);
    });
    child.once("close", (exitCode) => {
      signal?.removeEventListener("abort", abort);
      resolve({ stdout, stderr, exitCode: exitCode ?? 1 });
    });
  });
}

export default function (pi: ExtensionAPI) {
  pi.registerTool({
    name: "nemesis_review",
    label: "NEMESIS review",
    description:
      "Launch, inspect, or cancel an independent read-only NEMESIS reviewer. Reviewers run in a separate Pi RPC process with no inherited session.",
    parameters: Parameters,
    async execute(_toolCallId, params: Parameters, signal) {
      const args = params.operation === "verify"
        ? ["finding", "verify", ...(params.run_id ? [params.run_id] : []), ...(params.finding_id ? [params.finding_id] : [])]
        : ["review", params.operation];
      if (params.prompt) args.push("--prompt", params.prompt);
      if (params.operation !== "verify" && params.run_id) args.push(params.run_id);
      if (params.tail) args.push("--tail", String(params.tail));
      if (params.timeout_seconds) {
        args.push("--timeout-seconds", String(params.timeout_seconds));
      }
      const result = await runNemesis(args, signal);
      const text = result.stdout.trim() || result.stderr.trim() || "NEMESIS returned no output.";
      return {
        content: [{ type: "text", text }],
        details: { exitCode: result.exitCode, operation: params.operation },
        isError: result.exitCode !== 0,
      };
    },
  });
}
