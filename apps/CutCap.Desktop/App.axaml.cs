using Avalonia;
using Avalonia.Controls.ApplicationLifetimes;
using Avalonia.Markup.Xaml;
using CutCap.Desktop.Services;
using CutCap.Desktop.ViewModels;
using CutCap.Desktop.Views;

namespace CutCap.Desktop;

public sealed class App : Application
{
    private EngineHost? _engine;

    public override void Initialize() => AvaloniaXamlLoader.Load(this);

    public override void OnFrameworkInitializationCompleted()
    {
        if (ApplicationLifetime is IClassicDesktopStyleApplicationLifetime desktop)
        {
            _engine = new EngineHost();
            var viewModel = new MainWindowViewModel(_engine);
            var window = new MainWindow { DataContext = viewModel };
            desktop.MainWindow = window;
            _engine.Start();
            _engine.AttachRenderLoop(window);
            // After the window closes: stop pumping, then cancel media work and
            // join the native workers. FFmpeg children are reaped by the engine.
            desktop.Exit += (_, _) => _engine.Dispose();
        }
        base.OnFrameworkInitializationCompleted();
    }
}
