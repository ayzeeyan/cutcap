using Avalonia.Controls;
using CutCap.Desktop.Services;
using CutCap.Desktop.ViewModels;

namespace CutCap.Desktop.Views;

public partial class PlayerPanel : UserControl
{
    private PlayerViewModel? _viewModel;

    public PlayerPanel()
    {
        InitializeComponent();
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
