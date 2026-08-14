using System.Diagnostics;
using System.Runtime.InteropServices;

namespace DevConsole;

/// <summary>
/// Launches the repository's Dev*.cmd scripts so Visual Studio's F5 starts the
/// whole docker-compose stack.
/// </summary>
/// <remarks>
/// <para>
/// This process does no orchestration of its own. The scripts are the single
/// definition of what "start the stack" means, and duplicating any of it here
/// would create a second version that drifts. All this does is find them and
/// run them.
/// </para>
/// <para>
/// With no arguments it runs <c>DevConsole.cmd</c>, which starts the stack if
/// it is down and then opens the console page. That is the F5 path.
/// </para>
/// </remarks>
internal static class Program
{
    /// <summary>
    /// The scripts this launcher will run, keyed by the name passed on the
    /// command line.
    /// </summary>
    /// <remarks>
    /// An allow-list rather than "append whatever was typed to Dev*.cmd",
    /// because the value is handed to <c>cmd.exe</c>. A free-form name would
    /// let an argument choose an arbitrary program to execute.
    /// </remarks>
    private static readonly Dictionary<string, string> Scripts = new(StringComparer.OrdinalIgnoreCase)
    {
        ["console"] = "DevConsole.cmd",
        ["start"] = "DevStart.cmd",
        ["status"] = "DevStatus.cmd",
        ["logs"] = "DevLogs.cmd",
        ["demo"] = "DevDemo.cmd",
        ["replay"] = "DevReplay.cmd",
        ["stop"] = "DevStop.cmd",
        ["delete"] = "DevDelete.cmd",
        ["remove"] = "DevRemove.cmd",
    };

    /// <summary>What runs when no script is named — the F5 default.</summary>
    private const string DefaultScript = "console";

    /// <summary>
    /// Files that together identify the repository root.
    /// </summary>
    /// <remarks>
    /// Both are required. <c>compose.yaml</c> alone could match some other
    /// checkout that happens to sit above this one on disk.
    /// </remarks>
    private static readonly string[] RootMarkers =
    [
        "compose.yaml",
        "DemoRustMonoMicroservice.slnx",
    ];

    private static int Main(string[] args)
    {
        if (!OperatingSystem.IsWindows())
        {
            Console.Error.WriteLine(
                "DevConsole runs the repository's .cmd scripts through cmd.exe, so it is "
                + "Windows-only. On another platform run docker compose directly.");
            return 1;
        }

        // A bare "-h" should not have to find the repository first.
        if (args.Length > 0 && args[0] is "-h" or "--help" or "/?")
        {
            PrintUsage();
            return 0;
        }

        var (name, forwarded) = ParseArguments(args);

        if (!Scripts.TryGetValue(name, out var scriptFileName))
        {
            Console.Error.WriteLine($"[DevConsole] Unknown script '{name}'.");
            Console.Error.WriteLine();
            PrintUsage();
            return 1;
        }

        var repositoryRoot = FindRepositoryRoot();
        if (repositoryRoot is null)
        {
            Console.Error.WriteLine(
                "[DevConsole] Could not find the repository root. Looked for "
                + $"{string.Join(" and ", RootMarkers)} in every directory above "
                + $"{AppContext.BaseDirectory}.");
            return 1;
        }

        var scriptPath = Path.Combine(repositoryRoot, scriptFileName);
        if (!File.Exists(scriptPath))
        {
            Console.Error.WriteLine($"[DevConsole] {scriptFileName} is missing from {repositoryRoot}.");
            return 1;
        }

        return Run(scriptPath, repositoryRoot, forwarded);
    }

    /// <summary>
    /// Splits the command line into the script to run and the arguments to pass
    /// through to it.
    /// </summary>
    /// <remarks>
    /// A leading argument that starts with <c>-</c> or <c>/</c> is an option
    /// belonging to the default script, so <c>DevConsole --dry-run</c> forwards
    /// the flag. Anything else is read as a script name and must match one:
    /// treating an unrecognised name as an argument instead would make
    /// <c>DevConsole stat</c> quietly open the console page rather than report
    /// the typo.
    /// </remarks>
    private static (string Name, string[] Forwarded) ParseArguments(string[] args)
    {
        if (args.Length == 0 || args[0].StartsWith('-') || args[0].StartsWith('/'))
        {
            return (DefaultScript, args);
        }

        return (args[0], args[1..]);
    }

    /// <summary>
    /// Walks up from the executable's own location looking for the repository
    /// root.
    /// </summary>
    /// <remarks>
    /// Deliberately not the current working directory, which Visual Studio,
    /// <c>dotnet run</c> and a double-click in Explorer each set differently.
    /// The executable's path is the one thing that stays anchored to the
    /// checkout, whatever the output path or configuration.
    /// </remarks>
    /// <returns>The root, or <c>null</c> if the search reached the drive root.</returns>
    private static string? FindRepositoryRoot()
    {
        var directory = new DirectoryInfo(AppContext.BaseDirectory);

        while (directory is not null)
        {
            if (RootMarkers.All(marker => File.Exists(Path.Combine(directory.FullName, marker))))
            {
                return directory.FullName;
            }

            directory = directory.Parent;
        }

        return null;
    }

    /// <summary>
    /// Runs one script and returns its exit code.
    /// </summary>
    /// <remarks>
    /// <para>
    /// Nothing is redirected. With <see cref="ProcessStartInfo.RedirectStandardOutput"/>
    /// left off the child inherits this console directly, so a cold
    /// <c>DevStart.cmd</c> - two minutes of image pulls and health checks -
    /// prints as it happens instead of arriving in one lump at the end. It also
    /// keeps colour and any prompt the script chooses to show.
    /// </para>
    /// <para>
    /// Batch files cannot be executed by <c>CreateProcess</c>, so the script is
    /// run as an argument to <c>cmd.exe</c>. Each argument goes through
    /// <see cref="ProcessStartInfo.ArgumentList"/>, which quotes them
    /// individually - building one command-line string by hand is where
    /// paths containing spaces go wrong.
    /// </para>
    /// </remarks>
    [System.Runtime.Versioning.SupportedOSPlatform("windows")]
    private static int Run(string scriptPath, string workingDirectory, string[] forwarded)
    {
        // COMSPEC is where Windows records the command interpreter. Falling back
        // to a bare "cmd.exe" lets PATH resolve it if the variable is unset.
        var comSpec = Environment.GetEnvironmentVariable("COMSPEC") ?? "cmd.exe";

        var startInfo = new ProcessStartInfo
        {
            FileName = comSpec,
            WorkingDirectory = workingDirectory,
            UseShellExecute = false,
        };

        startInfo.ArgumentList.Add("/c");
        startInfo.ArgumentList.Add(scriptPath);
        foreach (var argument in forwarded)
        {
            startInfo.ArgumentList.Add(argument);
        }

        try
        {
            // Process implements IDisposable; `using` releases the handle even
            // if waiting throws.
            using var process = Process.Start(startInfo);

            if (process is null)
            {
                Console.Error.WriteLine($"[DevConsole] {comSpec} did not start.");
                return 1;
            }

            process.WaitForExit();

            // Forwarding the child's code is what lets this be used in a script
            // and what makes Visual Studio report a failed start as a failure.
            return process.ExitCode;
        }
        catch (Exception exception) when (exception is System.ComponentModel.Win32Exception or IOException)
        {
            Console.Error.WriteLine($"[DevConsole] Could not run {Path.GetFileName(scriptPath)}: {exception.Message}");
            return 1;
        }
    }

    private static void PrintUsage()
    {
        Console.WriteLine("DevConsole - starts the docker-compose development stack.");
        Console.WriteLine();
        Console.WriteLine("  DevConsole [script] [arguments...]");
        Console.WriteLine();
        Console.WriteLine($"With no arguments it runs {Scripts[DefaultScript]}: starts the stack if it is");
        Console.WriteLine("down, then opens the console page. This is what F5 does.");
        Console.WriteLine();
        Console.WriteLine("Scripts:");

        foreach (var (name, script) in Scripts)
        {
            Console.WriteLine($"  {name,-8} {script}");
        }

        Console.WriteLine();
        Console.WriteLine("Anything after the script name is passed straight through, so");
        Console.WriteLine("`DevConsole replay --dry-run` reaches DevReplay.cmd unchanged.");
    }
}
