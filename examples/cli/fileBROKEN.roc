app "file-io"
    packages { pf: "https://github.com/roc-lang/basic-cli/releases/download/0.9.0/oKWkaruh2zXxin_xfsYsCJobH1tO8_JvNkFzDwwzNUQ.tar.br" }
    imports [
        pf.Stdout,
        pf.Stderr,
        pf.Task.{ Task },
        pf.File,
        pf.Path,
        pf.Env,
        pf.Dir,
    ]
    provides [main] to pf

main : Task {} I32
main =
    path = Path.fromStr "out.txt"
    task =
        cwd <- Env.cwd |> Task.await
        cwdStr = Path.display cwd

        _ <- Stdout.line "cwd: $(cwdStr)" |> Task.await
        dirEntries <- Dir.list cwd |> Task.await
        contentsStr = Str.joinWith (List.map dirEntries Path.display) "\n    "

        _ <- Stdout.line "Directory contents:\n    $(contentsStr)\n" |> Task.await
        _ <- Stdout.line "Writing a string to out.txt" |> Task.await
        _ <- File.writeUtf8 path "a string!" |> Task.await
        contents <- File.readUtf8 path |> Task.await
        Stdout.line "I read the file back. Its contents: \"$(contents)\""

    Task.attempt task \result ->
        when result is
            Ok {} -> Stdout.line "Successfully wrote a string to out.txt"
            Err err ->
                msg =
                    when err is
                        FileWriteErr _ PermissionDenied -> "PermissionDenied"
                        FileWriteErr _ Unsupported -> "Unsupported"
                        FileWriteErr _ (Unrecognized _ other) -> other
                        FileReadErr _ _ -> "Error reading file"
                        _ -> "Uh oh, there was an error!"

                {} <- Stderr.line msg |> Task.await
                Task.err 1
