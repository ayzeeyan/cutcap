using System.Collections.ObjectModel;
using System.Runtime.CompilerServices;

[assembly: InternalsVisibleTo("CutCap.Desktop.Tests")]

namespace CutCap.Desktop.ViewModels;

internal static class Sync
{
    /// <summary>
    /// Updates <paramref name="target"/> in place to mirror <paramref name="source"/>,
    /// reusing items with the same key so list containers, keyboard focus and
    /// per-item view state (such as expansion) survive engine updates.
    /// </summary>
    public static void Items<TItem, TSource>(
        ObservableCollection<TItem> target,
        IReadOnlyList<TSource> source,
        Func<TItem, string> keyOfItem,
        Func<TSource, string> keyOfSource,
        Func<TSource, TItem> create,
        Action<TItem, TSource> update)
    {
        for (var index = 0; index < source.Count; index++)
        {
            var key = keyOfSource(source[index]);
            var existing = -1;
            for (var probe = index; probe < target.Count; probe++)
            {
                if (keyOfItem(target[probe]) == key)
                {
                    existing = probe;
                    break;
                }
            }
            if (existing < 0)
            {
                target.Insert(index, create(source[index]));
                continue;
            }
            if (existing != index)
            {
                target.Move(existing, index);
            }
            update(target[index], source[index]);
        }
        while (target.Count > source.Count)
        {
            target.RemoveAt(target.Count - 1);
        }
    }

    /// <summary>MM:SS.mmm of whole milliseconds, matching the engine's own notation.</summary>
    public static string Precise(long milliseconds)
    {
        var ms = Math.Max(0, milliseconds);
        return $"{ms / 60_000:00}:{ms / 1000 % 60:00}.{ms % 1000:000}";
    }
}
