codeunit 50100 "FX Codeunit"
{
    var
        GlobalLbl: Label 'Global %1', Comment = '%1 is a name';
        LockedLbl: Label 'Locked text', Locked = true;
        MaxLbl: Label 'Max text', MaxLength = 10;

    procedure Foo(): Text
    var
        LocalLbl: Label 'Local text';
    begin
        exit(LocalLbl);
    end;
}
