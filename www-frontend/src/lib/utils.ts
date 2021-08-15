export function ext(mime_type: string): string {
  if (mime_type === "video/mp4")
    return ".mp4";
  return "";
}

export function plural(n: number): string {
  return n === 1 ? "" : "s";
}
