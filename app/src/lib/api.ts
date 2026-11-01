// Typed wrappers around Tauri commands defined in src-tauri/src/lib.rs.
import { invoke } from "@tauri-apps/api/core";

export interface CrateInfo {
  name: string;
  version: string;
}

export interface Hello {
  message: string;
  app_version: string;
  crates: CrateInfo[];
}

export function hello(): Promise<Hello> {
  return invoke<Hello>("hello");
}
