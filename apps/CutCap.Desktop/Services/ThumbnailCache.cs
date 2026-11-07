using Avalonia.Media.Imaging;
using CutCap.Desktop.Controls;

namespace CutCap.Desktop.Services;

public enum ThumbnailState { Ready, NoFrame, Failed }

/// <summary>A decoded thumbnail (or a known no-frame/failed sample) and its actual PTS.</summary>
public sealed class ThumbnailEntry(ThumbnailState state, IDisposable? image, long actualTicks, int bytes)
{
    private IDisposable? _image = image;
    public ThumbnailState State { get; } = state;
    /// <summary>The drawable image (an Avalonia bitmap in the app), null for no-frame/failed or after eviction.</summary>
    public Bitmap? Bitmap => _image as Bitmap;
    public bool HasImage => _image is not null;
    public long ActualTicks { get; } = actualTicks;
    public int Bytes { get; } = bytes;
    internal void Release()
    {
        _image?.Dispose();
        _image = null;
    }
}

/// <summary>
/// Bounded in-memory filmstrip cache (Phase 4A), owned by the UI thread.
/// Least-recently-used eviction by resident pixel bytes: the default budget
/// (48 MiB, <c>CUTCAP_THUMBNAIL_CACHE_MB</c> overrides) holds about 2,000
/// 78×66 thumbnails. Known no-frame/failed samples are remembered (no bytes)
/// so they are not retried until their source key changes; relinking or a
/// changed source produces a new key, so stale images are never reused. One
/// bitmap is created per decoded thumbnail and reused for every draw until it
/// is evicted and disposed. Pending tokens are bounded by the demand size.
/// </summary>
public sealed class ThumbnailCache : IDisposable
{
    public const long DefaultBudgetBytes = 48L * 1024 * 1024;
    private const int MaxRemembered = 20_000;

    private readonly Dictionary<ThumbKey, LinkedListNode<(ThumbKey Key, ThumbnailEntry Entry)>> _entries = [];
    private readonly LinkedList<(ThumbKey Key, ThumbnailEntry Entry)> _lru = [];
    private readonly Dictionary<ulong, ThumbKey> _pending = [];
    private readonly Dictionary<ThumbKey, ulong> _tokens = [];
    private ulong _next = 1;

    public ThumbnailCache(long budgetBytes) => BudgetBytes = Math.Max(1024 * 1024, budgetBytes);

    public static ThumbnailCache FromEnvironment() =>
        new(long.TryParse(Environment.GetEnvironmentVariable("CUTCAP_THUMBNAIL_CACHE_MB"), out var mb) && mb > 0
            ? mb * 1024 * 1024 : DefaultBudgetBytes);

    public long BudgetBytes { get; }
    public long ResidentBytes { get; private set; }
    public long PeakResidentBytes { get; private set; }
    public int Count => _entries.Count;
    public int PendingCount => _pending.Count;
    public long Hits { get; private set; }
    public long Misses { get; private set; }
    public long Evictions { get; private set; }

    /// <summary>Looks up a thumbnail and marks it recently used.</summary>
    public bool TryGet(ThumbKey key, out ThumbnailEntry entry)
    {
        if (_entries.TryGetValue(key, out var node))
        {
            _lru.Remove(node);
            _lru.AddFirst(node);
            entry = node.Value.Entry;
            Hits++;
            return true;
        }
        Misses++;
        entry = null!;
        return false;
    }

    public bool Contains(ThumbKey key) => _entries.ContainsKey(key);

    /// <summary>A stable token for a requested key (the same while it is pending).</summary>
    public ulong TokenFor(ThumbKey key)
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

    /// <summary>
    /// Stores a result for a pending token. Returns false (and disposes the
    /// image) when the token is unknown, i.e. no longer demanded. The image is
    /// a bitmap in the app; any disposable resource is budgeted the same way.
    /// </summary>
    public bool Complete(ulong token, ThumbnailState state, IDisposable? bitmap, long actualTicks, int bytes)
    {
        if (!_pending.Remove(token, out var key))
        {
            bitmap?.Dispose();
            return false;
        }
        _tokens.Remove(key);
        if (_entries.Remove(key, out var old))
        {
            _lru.Remove(old);
            ResidentBytes -= old.Value.Entry.Bytes;
            old.Value.Entry.Release();
        }
        var entry = new ThumbnailEntry(state, bitmap, actualTicks, bitmap is null ? 0 : bytes);
        _entries[key] = _lru.AddFirst((key, entry));
        ResidentBytes += entry.Bytes;
        PeakResidentBytes = Math.Max(PeakResidentBytes, ResidentBytes);
        Evict();
        return true;
    }

    private void Evict()
    {
        while ((ResidentBytes > BudgetBytes || _entries.Count > MaxRemembered) && _lru.Last is { } last)
        {
            _lru.RemoveLast();
            _entries.Remove(last.Value.Key);
            ResidentBytes -= last.Value.Entry.Bytes;
            last.Value.Entry.Release();
            Evictions++;
        }
    }

    /// <summary>Project replacement: pending requests are void; decoded images stay keyed by source.</summary>
    public void ClearPending()
    {
        _pending.Clear();
        _tokens.Clear();
    }

    public void Dispose()
    {
        foreach (var (_, entry) in _lru) entry.Release();
        _lru.Clear();
        _entries.Clear();
        ClearPending();
        ResidentBytes = 0;
    }
}
