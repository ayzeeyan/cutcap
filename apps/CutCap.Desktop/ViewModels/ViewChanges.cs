using CutCap.Interop;

namespace CutCap.Desktop.ViewModels;

/// <summary>
/// Which parts of a new engine view differ from the previous one. During
/// playback the clock changes every pump, so only the player should update;
/// the library, inspector and timeline are re-applied only when their own data
/// changed. DTO lists are fresh instances on every read, so lists are compared
/// by elements (records compare by value).
/// </summary>
public readonly record struct ViewChanges(bool Library, bool Inspector, bool Timeline)
{
    public static readonly ViewChanges All = new(true, true, true);

    public static ViewChanges Between(SessionView? previous, SessionView next)
    {
        if (previous is null)
        {
            return All;
        }
        var library = previous.Import != next.Import || !previous.Media.SequenceEqual(next.Media);
        var inspector = previous.Inspector.Kind != next.Inspector.Kind
            || previous.Inspector.Title != next.Inspector.Title
            || previous.Inspector.Detail != next.Inspector.Detail
            || !SameSections(previous.Inspector.Sections, next.Inspector.Sections);
        // Clips arrive in a separate document whose generation is in the view.
        var timeline = previous.EngineReady != next.EngineReady || !previous.Tracks.SequenceEqual(next.Tracks)
            || previous.TimelineGeneration != next.TimelineGeneration || previous.History != next.History
            || !previous.Media.SequenceEqual(next.Media);
        return new ViewChanges(library, inspector, timeline);
    }

    private static bool SameSections(IReadOnlyList<SectionView> a, IReadOnlyList<SectionView> b)
    {
        if (a.Count != b.Count)
        {
            return false;
        }
        for (var i = 0; i < a.Count; i++)
        {
            if (a[i].Title != b[i].Title || !a[i].Rows.SequenceEqual(b[i].Rows))
            {
                return false;
            }
        }
        return true;
    }
}
