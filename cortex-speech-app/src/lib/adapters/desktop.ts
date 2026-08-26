import { convertFileSrc as tauriConvertFileSrc } from '@tauri-apps/api/core';
import { listen as tauriListen, type Event, type UnlistenFn } from '@tauri-apps/api/event';

/**
 * Platform adapter for window, dialog, event and asset mechanics.
 *
 * Keep platform mechanics here so components and stores depend on stable application-facing
 * functions. Raw command transport lives separately in the closed legacy IPC adapter; generated
 * Specta bindings remain the Rust-authored command contract.
 */
export type DesktopEvent<T> = Event<T>;
export type DesktopUnlisten = UnlistenFn;

export function listen<T>(event: string, handler: (event: Event<T>) => void): Promise<UnlistenFn> {
  return tauriListen<T>(event, handler);
}

export function desktopAssetUrl(path: string): string {
  return tauriConvertFileSrc(path);
}

export interface DialogFilter {
  name: string;
  extensions: string[];
}

export interface SaveFileOptions {
  title?: string;
  defaultPath?: string;
  filters?: DialogFilter[];
}

export interface OpenFileOptions {
  title?: string;
  filters?: DialogFilter[];
}

export async function saveFile(options: SaveFileOptions): Promise<string | null> {
  const { save } = await import('@tauri-apps/plugin-dialog');
  return save(options);
}

export async function chooseDirectory(title?: string): Promise<string | null> {
  const { open } = await import('@tauri-apps/plugin-dialog');
  const selected = await open({ directory: true, multiple: false, title });
  return typeof selected === 'string' ? selected : null;
}

export async function chooseFile(options: OpenFileOptions = {}): Promise<string | null> {
  const { open } = await import('@tauri-apps/plugin-dialog');
  const selected = await open({ ...options, directory: false, multiple: false });
  return typeof selected === 'string' ? selected : null;
}

export interface DesktopCloseRequestedEvent {
  preventDefault(): void;
}

export interface DesktopWindow {
  onCloseRequested(
    handler: (event: DesktopCloseRequestedEvent) => void | Promise<void>,
  ): Promise<UnlistenFn>;
  destroy(): Promise<void>;
  close(): Promise<void>;
}

export async function currentDesktopWindow(): Promise<DesktopWindow> {
  const { getCurrentWindow } = await import('@tauri-apps/api/window');
  return getCurrentWindow();
}
