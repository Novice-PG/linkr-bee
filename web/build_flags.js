/* Build-time switches for the plain web terminal.
 *
 * The plain web path has no bundler, so a variant is produced by copying web/
 * and overwriting this one file: tools/build_pages.sh does that for GitHub
 * Pages, where an https page cannot open the ws:// that the LAN bridge needs.
 * The committed file is the browser default, and mobile builds keep it too.
 * Every switch read from here must degrade to today's behaviour when true, so
 * the default stays the variant with the fewest surprises.
 */
export const buildFlags = { lanBridge: true };
