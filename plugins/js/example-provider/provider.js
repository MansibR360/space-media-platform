/**
 * Reference provider.
 *
 * Read this to learn the contract, then copy it. Everything a provider needs
 * is here: the exported function, the arguments it receives, `fetch`, and the
 * shape it must return.
 *
 * Disabled in its manifest — flip `enabled` to true to see it in the app. It
 * returns Apple's public sample stream, so playback can be proven end to end
 * without depending on any scraping target.
 */

/**
 * The one function a provider must export.
 *
 * @param {string} tmdbId    TMDB id, e.g. "27205"
 * @param {string} mediaType "movie" or "tv"
 * @param {number|null} season   1-based, null for films
 * @param {number|null} episode  1-based, null for films
 * @returns {Promise<Array>} streams: { name, title, url, quality, headers? }
 */
async function getStreams(tmdbId, mediaType, season, episode) {
  // `fetch` works as you would expect, including await and res.json().
  // Real providers use it to reach an upstream API and dig out a stream url.
  const meta = await lookupTitle(tmdbId, mediaType)

  const label =
    mediaType === 'tv' ? `${meta.title} S${season}E${episode}` : meta.title

  return [
    {
      name: 'Example',
      title: label,
      // A direct url. This is the point of a provider: the app plays this
      // itself rather than sending the viewer off to a website.
      url: 'https://devstreaming-cdn.apple.com/videos/streaming/examples/img_bipbop_adv_example_fmp4/master.m3u8',
      quality: '1080p',
      // Optional. Sent with the stream request when the host needs them.
      headers: { Referer: 'https://example.com/' },
    },
  ]
}

/** Small helper, to show that ordinary async code is fine in here. */
async function lookupTitle(tmdbId, mediaType) {
  try {
    const res = await fetch(`https://api.themoviedb.org/3/${mediaType}/${tmdbId}`)
    if (!res.ok) return { title: `TMDB ${tmdbId}` }
    const data = await res.json()
    return { title: data.title || data.name || `TMDB ${tmdbId}` }
  } catch (err) {
    // A provider that cannot reach its upstream should degrade, not throw:
    // throwing marks the whole provider as failing for this request.
    console.warn('metadata lookup failed:', err)
    return { title: `TMDB ${tmdbId}` }
  }
}
