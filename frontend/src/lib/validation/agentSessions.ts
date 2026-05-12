import { z } from "zod";

const RUNTIME_KINDS = ["claude_code", "opencode"] as const;

export const createSessionFormSchema = z.object({
  runtime: z.enum(RUNTIME_KINDS, {
    message: "Runtime is required",
  }),
  initial_prompt: z
    .string()
    .max(4096, "Prompt must be under 4,096 characters")
    .optional()
    .or(z.literal("")),
  workspace_path: z
    .string()
    .max(512, "Workspace path must be under 512 characters")
    .optional()
    .or(z.literal("")),
});

export type CreateSessionFormValues = z.infer<typeof createSessionFormSchema>;

export const RUNTIME_OPTIONS: ReadonlyArray<{
  readonly value: (typeof RUNTIME_KINDS)[number];
  readonly label: string;
}> = [
  { value: "claude_code", label: "Claude Code" },
  { value: "opencode", label: "OpenCode" },
];
