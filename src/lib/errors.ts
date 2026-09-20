export function errorMessage(error: unknown, unknownError: string): string {
  if (error instanceof Error) return error.message;
  if (typeof error === "string") return error;
  return unknownError;
}
