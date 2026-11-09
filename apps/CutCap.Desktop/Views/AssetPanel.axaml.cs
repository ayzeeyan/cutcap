using Avalonia;
using Avalonia.Controls;
using Avalonia.Input;
using Avalonia.Interactivity;
using Avalonia.VisualTree;
using CutCap.Desktop.Controls;
using CutCap.Desktop.ViewModels;

namespace CutCap.Desktop.Views;

/// <summary>
/// View glue only. A press on a media tile followed by a small drag starts a
/// drag-and-drop with an in-process payload (media identity, name, duration,
/// readiness); the timeline lanes decide where it would go and the engine
/// decides whether it is accepted. A plain click still selects the tile.
/// </summary>
public partial class AssetPanel : UserControl
{
    private PointerPressedEventArgs? _press;
    private Point _pressedAt;
    private MediaItemViewModel? _item;
    private bool _dragging;

    public AssetPanel()
    {
        InitializeComponent();
        MediaGrid.AddHandler(PointerPressedEvent, OnTilePressed, RoutingStrategies.Tunnel);
        MediaGrid.AddHandler(PointerMovedEvent, OnTileMoved, RoutingStrategies.Tunnel);
        MediaGrid.AddHandler(PointerReleasedEvent, OnTileReleased, RoutingStrategies.Tunnel);
    }

    private void OnTilePressed(object? sender, PointerPressedEventArgs e)
    {
        if (!e.GetCurrentPoint(MediaGrid).Properties.IsLeftButtonPressed) return;
        _item = (e.Source as Visual)?.GetSelfAndVisualAncestors().OfType<Control>()
            .Select(c => c.DataContext).OfType<MediaItemViewModel>().FirstOrDefault();
        _press = _item is null ? null : e;
        _pressedAt = e.GetPosition(MediaGrid);
    }

    /// <summary>
    /// The current source stays selected in the list while a clip or track owns
    /// the inspector, so the list reports no change; a plain click brings it back.
    /// </summary>
    private void OnTileReleased(object? sender, PointerReleasedEventArgs e)
    {
        if (_press is not null && _item is { } item && DataContext is MainWindowViewModel shell)
        {
            shell.Library.ShowSource(item);
        }
        (_press, _item) = (null, null);
    }

    private async void OnTileMoved(object? sender, PointerEventArgs e)
    {
        if (_dragging || _press is not { } press || _item is not { } item) return;
        var travel = e.GetPosition(MediaGrid) - _pressedAt;
        if (Math.Abs(travel.X) < TimelineGestures.DragThreshold * 2 && Math.Abs(travel.Y) < TimelineGestures.DragThreshold * 2) return;
        _press = null;
        var payload = new MediaDrag(item.Id, item.Name, item.Media.DurationMs, item.Media.Availability == "ready", item.Media.AudioOnly);
        var data = new DataTransfer();
        data.Add(DataTransferItem.Create(TimelineLanes.MediaDragFormat, payload));
        _dragging = true;
        try
        {
            await DragDrop.DoDragDropAsync(press, data, DragDropEffects.Copy);
        }
        finally
        {
            _dragging = false;
            _item = null;
        }
    }
}
