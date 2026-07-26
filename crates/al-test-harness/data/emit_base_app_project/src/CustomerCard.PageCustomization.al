pagecustomization "Emitter Customer Card" customizes "Customer Card"
{
    layout
    {
        modify(Name)
        {
            Visible = true;
        }
        addlast(General)
        {
            field("Customized Emitter Note"; Rec."Emitter Note")
            {
                ApplicationArea = All;
                Caption = 'Customized Emitter Note';
            }
        }
    }
}
