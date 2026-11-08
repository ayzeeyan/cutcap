namespace CutCap.Desktop.Services;

/// <summary>Cache identity of one analysed waveform chunk: verified source stream and chunk index.</summary>
public readonly record struct WaveKey(string AudioKey, long Chunk);

/// <summary>
/// Measured peaks of one chunk: <c>Buckets</c> interleaved (min, max) pairs,
/// each over <c>BucketFrames</c> native-rate samples of every channel. A
/// bucket with no decoded samples has min &gt; max. Failed chunks carry none.
/// </summary>
public sealed class WaveChunk(float[]? peaks, int buckets, int bucketFrames, uint rate)
{
    public float[]? Peaks { get; } = peaks;
    public int Buckets { get; } = buckets;
    public int BucketFrames { get; } = bucketFrames;
    public uint Rate { get; } = rate;
    public int Bytes => (Peaks?.Length ?? 0) * sizeof(float);
}

/// <summary>
/// Bounded in-memory waveform cache (Phase 4B), owned by the UI thread.
/// Least-recently-used eviction by resident bytes: the default budget
/// (16 MiB, <c>CUTCAP_WAVEFORM_CACHE_MB</c> overrides) holds about 2,000
/// chunks of 1,024 buckets — some three hours of 48 kHz audio at every zoom,
/// because each chunk is reused across zoom levels. Keys include the verified
/// source and stream, so relinking never reuses another file's peaks.
/// </summary>
public sealed class WaveformCache
{
    public const long DefaultBudgetBytes = 16L * 1024 * 1024;
    private const int MaxRemembered = 20_000;

    private readonly Dictionary<WaveKey, LinkedListNode<(WaveKey Key, WaveChunk Chunk)>> _entries = [];
    private readonly LinkedList<(WaveKey Key, WaveChunk Chunk)> _lru = [];
    private readonly Dictionary<ulong, WaveKey> _pending = [];
    private readonly Dictionary<WaveKey, ulong> _tokens = [];
    private ulong _next = 1;

    public WaveformCache(long budgetBytes) => BudgetBytes = Math.Max(256 * 1024, budgetBytes);

    public static WaveformCache FromEnvironment() =>
        new(long.TryParse(Environment.GetEnvironmentVariable("CUTCAP_WAVEFORM_CACHE_MB"), out var mb) && mb > 0
            ? mb * 1024 * 1024 : DefaultBudgetBytes);

    public long BudgetBytes { get; }
    public long ResidentBytes { get; private set; }
    public int Count => _entries.Count;
    public int PendingCount => _pending.Count;
    public long Hits { get; private set; }
    public long Misses { get; private set; }
    public long Evictions { get; private set; }

    /// <summary>Looks up a chunk and marks it recently used.</summary>
    public bool TryGet(WaveKey key, out WaveChunk chunk)
    {
        if (_entries.TryGetValue(key, out var node))
        {
            _lru.Remove(node);
            _lru.AddFirst(node);
            chunk = node.Value.Chunk;
            Hits++;
            return true;
        }
        Misses++;
        chunk = null!;
        return false;
    }

    public bool Contains(WaveKey key) => _entries.ContainsKey(key);

    /// <summary>A stable token for a requested key (the same while it is pending).</summary>
    public ulong TokenFor(WaveKey key)
    {
        if (_tokens.TryGetValue(key, out var token)) return token;
        token = _next++;
        _tokens[key] = token;
        _pending[token] = key;
        return token;
    }

    /// <summary>Forgets pending tokens that are no longer demanded (their results are ignored).</summary>
    public void RetainPending(IReadOnlySet<ulong> wanted)
    {
        foreach (var token in _pending.Keys.Where(t => !wanted.Contains(t)).ToArray())
        {
            _tokens.Remove(_pending[token]);
            _pending.Remove(token);
        }
    }

    /// <summary>Stores a result for a pending token; false when the token is no longer demanded.</summary>
    public bool Complete(ulong token, WaveChunk chunk)
    {
        if (!_pending.Remove(token, out var key)) return false;
        _tokens.Remove(key);
        if (_entries.Remove(key, out var old))
        {
            _lru.Remove(old);
            ResidentBytes -= old.Value.Chunk.Bytes;
        }
        _entries[key] = _lru.AddFirst((key, chunk));
        ResidentBytes += chunk.Bytes;
        while ((ResidentBytes > BudgetBytes || _entries.Count > MaxRemembered) && _lru.Last is { } last)
        {
            _lru.RemoveLast();
            _entries.Remove(last.Value.Key);
            ResidentBytes -= last.Value.Chunk.Bytes;
            Evictions++;
        }
        return true;
    }

    /// <summary>Project replacement: pending requests are void; analysed chunks stay keyed by source.</summary>
    public void ClearPending()
    {
        _pending.Clear();
        _tokens.Clear();
    }
}
