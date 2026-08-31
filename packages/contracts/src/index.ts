import { z } from "zod";

export const HealthResponseSchema = z.object({
  status: z.literal("ok"),
  apiVersion: z.literal("v1"),
  applicationVersion: z.string()
});

export const SystemInfoResponseSchema = z.object({
  status: z.literal("ok"),
  apiVersion: z.literal("v1"),
  applicationVersion: z.string(),
  compatibility: z.object({
    minimumWebVersion: z.string(),
    maximumWebMajorVersion: z.number().int().nonnegative()
  })
});

export type HealthResponse = z.infer<typeof HealthResponseSchema>;
export type SystemInfoResponse = z.infer<typeof SystemInfoResponseSchema>;
