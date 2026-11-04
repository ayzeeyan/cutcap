using Avalonia.Controls;
using Avalonia.Input;
using Avalonia.Interactivity;
using CutCap.Desktop.Services;
using CutCap.Desktop.ViewModels;

namespace CutCap.Desktop.Views;

public partial class PlayerPanel : UserControl
{
    private PlayerViewModel? _viewModel;

    public PlayerPanel()
    {
        InitializeComponent();
        // Tunnel so a drag is marked before the slider moves its value on press.
        Scrubber.AddHandler(PointerPressedEvent, (_, _) => _viewModel?.BeginScrub(), RoutingStrategies.Tunnel, handledEventsToo: true);
        Scrubber.AddHandler(PointerReleasedEvent, (_, _) => _viewModel?.EndScrub(), RoutingStrategies.Tunnel | RoutingStrategies.Bubble, handledEventsToo: true);
        Scrubber.AddHandler(PointerCaptureLostEvent, (_, _) => _viewModel?.EndScrub(), RoutingStrategies.Bubble, handledEventsToo: true);
    }

    protected override void OnDataContextChanged(EventArgs e)
    {
        base.OnDataContextChanged(e);
        if (_viewModel is not null)
        {
            _viewModel.FrameChanged -= OnFrameChanged;
        }
        _viewModel = DataContext as PlayerViewModel;
        if (_viewModel is not null)
        {
            _viewModel.FrameChanged += OnFrameChanged;
        }
    }

    private void OnFrameChanged(object? sender, EventArgs e)
    {
        if (_viewModel?.Session is { IsDisposed: false } session)
        {
            var before = Frame.Generation;
            Frame.Present(session);
            if (Frame.Generation != before)
            {
                PreviewTrace.Presented(Frame.Generation, Frame.LastCopyMicroseconds);
            }
        }
    }
}
