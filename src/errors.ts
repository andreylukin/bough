/**
 * A domain error that maps directly to an HTTP response: `status` + message.
 * The route dispatcher (server/app.ts) turns any thrown HttpError into that
 * response in ONE place, so domain modules throw typed subclasses and handlers
 * carry no per-error catch blocks.
 */
export class HttpError extends Error {
  constructor(readonly status: number, message: string) {
    super(message);
    this.name = new.target.name;
  }
}
