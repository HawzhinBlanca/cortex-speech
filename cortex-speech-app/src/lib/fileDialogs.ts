import {
  chooseDirectory as chooseDesktopDirectory,
  chooseFile as chooseDesktopFile,
  saveFile as saveDesktopFile,
  type OpenFileOptions,
  type SaveFileOptions,
} from './adapters/desktop';

/** Application-facing file selection service. Components never import a Tauri plugin directly. */
export function chooseDirectory(title?: string): Promise<string | null> {
  return chooseDesktopDirectory(title);
}

export function chooseFile(options: OpenFileOptions = {}): Promise<string | null> {
  return chooseDesktopFile(options);
}

export function saveFile(options: SaveFileOptions): Promise<string | null> {
  return saveDesktopFile(options);
}
