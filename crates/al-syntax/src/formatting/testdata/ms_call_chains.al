codeunit 50100 "Probe Two"
{
    procedure A()
    var
        B: Codeunit "Foo";
    begin
        B.Setup()
        .Add(1)
        .Add(2)
        .Show();
        if X then
            B.Setup()
            .Add(1)
            .Show();
        if X then begin
            B.Setup()
            .Add(1)
            .Show(B.Setup()
            .Add(2));
        end;
        B.Setup().Add(1)
        .Add(2)
        // comment
        .Show();
        Result := B.Setup()
        .Add(1);
    end;
}

