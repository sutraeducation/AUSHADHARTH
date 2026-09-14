/**
 * This PC's calendar date as `YYYY-MM-DD`.
 *
 * The Store Service remains authoritative for every stored timestamp; this is only the date a
 * screen offers as a default or asks a question "as of". It must come from the workstation clock
 * rather than UTC: an Indian pharmacy trading at 00:19 IST is on a calendar day that UTC has not
 * reached, and a UTC date would quietly ask yesterday's question for the first five and a half
 * hours of every day.
 */
export function businessToday(): string {
  const now = new Date();
  const month = String(now.getMonth() + 1).padStart(2, "0");
  const day = String(now.getDate()).padStart(2, "0");
  return `${now.getFullYear()}-${month}-${day}`;
}
